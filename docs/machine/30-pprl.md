<!-- machine-first: generated from source audit 2026-07-29; audience=agents -->

# PPRL — PII Pseudonymization Layer

## SCOPE

This document specifies the privacy-preserving record-linkage (PPRL) layer that
tokenizes personal data before it reaches any cloud model or external-facing
surface. It is descriptive of the shipped code in
`core/src/utils/anonymizer.rs` (the token primitive), `wms/src/mcp/tools.rs` and
`wms/src/mcp/reveal.rs` (the MCP tokenize/mask/reveal paths), and
`wms/src/ai/pii_policy.rs` (the egress policy). Local NER is an optional,
feature-gated augmentation (`wms/src/ai/local_ner.rs`).

The design goal is a single, stable **pseudonym token** that (a) never exposes
the clear value, (b) is identical for identical inputs across every node and
every record, so callers can correlate and pivot on it, and (c) is reversible to
clear text **only** on the node that holds the raw data and **only** for a
master-tier caller, into a local file — with no persistent plaintext vault.

---

## 1. Token grammar

A token is `<Type>_<16 uppercase hex>`, produced by
`anonymizer.rs::obfuscate_pii(text, pii_type)` as `format!("{}_{:016X}", type,
simhash(text))`. The 16 hex digits are the 64-bit SimHash of the value.

Recognized types (`anonymizer.rs::token_regex`):
`Name`, `Email`, `Phone`, `Address`, `Company`, `Iban`, `VatId`, `Card`.
Examples of the shape (not real values): `Name_5AC7269A88303052`,
`Email_D7EACC17A65A5553`, `Phone_0123456789ABCDEF`.

`anonymizer.rs::parse_pii_token(s)` returns the type label iff `s` (trimmed) is
**exactly** one token — used by query surfaces to route token-shaped input to an
exact-match path instead of full-text/vector search, where a hash token would
only produce noise. `anonymizer.rs::extract_pii_tokens(text)` collects every
token embedded in text (deduped, first-seen order); it is a pure scan (no key
needed), so any node can derive a document's token index from already-masked
content.

### 1.1 The token primitive: keyed SimHash

`anonymizer.rs::simhash(text)`:

- Lowercases the input, takes character **bigrams**; for each bigram it computes
  `SHA-256(bigram || pepper)` and folds the top 64 bits into a signed
  per-bit accumulator; the final token bit is set where the accumulator is
  positive. (Inputs shorter than one bigram fall back to a keyed SHA-256 of the
  whole value.)
- The **pepper** comes from the env var `SYNC_SECRET` and has **no default**. If
  it is unset the function panics/refuses rather than emit a reversible token.

Two guaranteed properties follow from the construction:

- **Determinism:** same input + same pepper → same 64-bit token. This is what
  makes a token a stable cross-record, cross-node key.
- **Similarity preservation:** near-identical inputs (a transliteration, a minor
  spelling variant) produce tokens with low Hamming distance. This is a
  deliberate linkage feature, not a leak of the clear value.

The pepper's role is to defeat a dictionary attack: without a secret mixed into
every bigram hash, an attacker could pre-hash candidate values and match tokens.
This document describes only that the pepper exists and comes from `SYNC_SECRET`;
its value is deployment secret material.

---

## 2. The core invariant: PII never leaves the node in clear toward cloud

Every cloud-model call site and every external surface masks first.

- **MCP surface** (`wms/src/mcp/tools.rs`): every tool result runs identity
  fields through `tools.rs::tokenize(state, value, type)` (structured fields:
  name/email/phone/address) or `tools.rs::mask_free_text(state, text)` (message
  bodies). No clear PII ever leaves an MCP tool result, for **every** caller
  tier — tokenization is unconditional defense-in-depth, not a per-tier toggle.
- **AI egress** (`wms/src/ai/*`): summarization and embedding inputs are masked
  before the prompt/vector text is sent, subject to the egress policy (§5).

### 2.1 Structured fields vs free-text scrubbing

Structured fields have a known type and are tokenized directly (`tokenize`).
Free text is scrubbed in layers by `mask_free_text`:

1. **Heuristic person-name masking** —
   `anonymizer.rs::mask_person_names_heuristic`: masks capitalized multi-word
   name spans as a single `Name_` token, gated by a compiled first-name lexicon,
   a surname stoplist (common domain nouns), an organization/brand veto (so brand
   names stay clear), and a `plausible_person_name` gate (rejects ordinary
   multi-word phrases that merely happen to be capitalized). Idempotent over
   already-baked tokens.
2. **Deterministic regex backstop** — `anonymizer.rs::scrub_pii_regex`
   (`pii_patterns`): high-confidence structured PII an extractor can miss —
   emails, IBANs, phone numbers, payment cards, VAT-IDs. Patterns are
   conservative (phone/card require `+` or explicit separators) so bare digit
   runs (serial numbers, order numbers) are **not** masked and search recall is
   preserved. Each match becomes the same `obfuscate_pii` token as every other
   path.
3. **Optional local NER** — when built with the local-embed feature and
   `ECK_PII_NER=local`, `wms/src/ai/local_ner.rs` adds an on-device
   named-entity pass over prose (e.g. subject lines) that the lexicon backstop
   would miss, minting the same deterministic `Name_` tokens. Absent that, the
   lexicon/heuristic is the fallback.

Because every path routes through `obfuscate_pii` with the same pepper, a name
in a message body collapses to the **identical** `Name_<hex>` token as the same
person's structured `author` field.

---

## 3. Tokens as query keys

Because a token is a deterministic function of (value, type, pepper), the same
person always yields the same token in every record and on every node. Callers
(including AI agents on the MCP surface) therefore:

- **Correlate** across results without ever seeing a clear value — two rows
  carrying `Name_5AC7…` refer to the same person.
- **Pivot** by passing a token back as a query argument. The agent-facing tools
  match a token against the tokenized form of stored fields (they re-run
  `obfuscate_pii` over the candidate clear value and compare, e.g. in
  `tools.rs`'s customer lookup), so a token is a valid exact-match key over every
  record referencing that person — with the clear value never leaving the node.

Free-text bodies additionally mask bare first-name references
(`tools.rs::thread_name_needles` + `mask_known_names`) by using the structured
participants already known for the thread, mapping a bare part to the token of
the full name so correlation and reveal keep working.

---

## 4. Reveal flow

Reveal is **"find the value that produced this token"**, not "decode the hash".
The hash is one-way; reveal works off a recorded or re-derivable
`token → clear` map. Master tier only.

### 4.1 The `pii_reveal` store (`wms/src/mcp/reveal.rs::PiiRevealStore`)

- A `token → clear` reverse map recorded at **tokenization time**: every
  `tokenize` / `mask_free_text` call also calls `pii_reveal.record(token,
  value)`.
- **RAM only.** Deliberately no persistent plaintext vault (that would recreate
  the exact PII-at-rest exposure tokens exist to avoid). Bounded FIFO,
  drop-oldest, default cap `DEFAULT_CAP = 50_000`. An evicted or foreign-node
  token simply reveals as *unresolved*.
- `record` is idempotent (deterministic tokens); `resolve(token)` returns the
  clear value if known on **this** node.

### 4.2 The reveal methods

- `reveal.rs::reveal_tokens_method(store, tier, params)` — the **only** path that
  returns clear PII, and only to `McpTier::Master`. It is a JSON-RPC *method*,
  not a tool, so it never appears in `tools/list` for any tier; an agent caller
  is refused (`-32001`). Returns `{revealed: {token: value}, unresolved: [token]}`.
- **DB fallback** (`reveal.rs::resolve_from_db`, gated behind the master check):
  tokens baked into stored summaries/subjects at distillation time were never
  passed through a live tool, so the RAM store has never seen them. The fallback
  finds documents whose `pii_fingerprints` index contains the token, re-derives
  their full reveal dictionary from the raw payload + metadata (the same
  `derive_reveal_pairs` the staff-display path uses — **no** plaintext vault,
  re-hash to reproduce the identical token), and records the pairs. On a thin or
  cache node with no raw data the token stays unresolved.
- **File reveal** (`tools.rs`: `reveal_file_local`, master token only): resolves
  every token inside a server-local file (including `Company_<hex>` tokens inside
  stored summary text) and writes a revealed copy locally (optionally to a sibling
  path), never returning clear values over the wire.

---

## 5. Egress policy (`wms/src/ai/pii_policy.rs`)

Two independent surfaces, each with an env knob: `ECK_PII_EMBED_POLICY`
(embedding egress) and `ECK_PII_LLM_POLICY` (generative-prompt egress). Three
tiers (`PiiPolicy`): `masked` (default), `clear_local`, `clear`.

- `masked` — always tokenize before the surface, regardless of backend. Unknown
  or absent values resolve here (**fail closed**: a typo never disables masking).
- `clear_local` — clear text allowed only when that surface's backend runs
  on-prem (`ECK_EMBED_MODE=local` / reserved `ECK_LLM_MODE=local`); with a cloud
  backend it silently degrades to `masked`.
- `clear` — clear text even to a cloud backend; a deliberate controller decision
  for a no-secrets deployment, logged loudly (rate-limited to once per surface).

The decision is **per mesh** (like `SYNC_SECRET`): summaries and vectors sync
mesh-wide, so mixing masked and clear authors in one mesh poisons both the vector
space and the "stored summary is masked" assumption downstream surfaces rely on.
The policy does **not** change MCP-output tokenization (always on) nor
`pii_fingerprints` derivation (always on — it is the erasure index key, not an
egress).

---

## 6. Operational contract

- **Protect the pepper.** `SYNC_SECRET` is the linkage key for the whole mesh. It
  must be identical on every node of one mesh and never shipped in client code or
  logs. There is no in-code default; an unset pepper halts anonymization.
- **Rotation breaks linkage.** Changing `SYNC_SECRET` re-derives every token, so:
  previously stored tokens (in summaries, `pii_fingerprints`, embeddings' masked
  source) become **orphaned** — a new tokenization of the same person no longer
  matches the old token, correlation across pre/post-rotation records is lost,
  and DB-fallback reveal of old baked tokens fails (re-hashing the raw value now
  yields a different token). Rotation therefore requires re-deriving stored tokens
  from raw data, and is a mesh-wide, not per-node, operation.

---

## GUARANTEES

1. Same clear value + same `pii_type` + same pepper → identical token, on every
   node and in every record (deterministic, keyed).
2. A token never contains or encodes the clear value; recovering the value
   requires a recorded or re-derivable `token → clear` map, which exists only on
   a node that holds the raw data.
3. Tokenization is unconditional on the MCP surface for every caller tier; no
   clear PII leaves a tool result.
4. Clear reveal is reachable only by a master-tier caller and only via the
   `reveal_tokens` method / `reveal_file_local` tool, which write clear values to
   local files, never over the wire; an agent-tier caller is refused.
5. No persistent plaintext vault exists; the reveal map is RAM-only and bounded,
   and DB-fallback reveal re-derives tokens from raw data rather than reading a
   stored clear copy.
6. The egress policy fails closed: an unknown/absent value, or an unknown policy
   string, resolves to `masked`.
7. `pii_fingerprints` (the erasure/correlation index) and MCP tokenization stay
   on even under a `clear` egress policy.

## NON-GUARANTEES

1. **SimHash similarity is deliberate linkage, not encryption.** Near-identical
   inputs yield near-identical tokens (low Hamming distance) on purpose. Tokens
   leak *similarity structure*; they are not confidentiality-grade ciphertext.
2. **Not collision-free.** A 64-bit SimHash can collide, and similarity
   preservation intentionally clusters related values; two different inputs may
   share or neighbor a token.
3. **Brute-forceable for tiny input spaces.** An attacker who knows the pepper,
   or who can guess it, can hash a candidate set and match tokens; even without
   the pepper, a small/enumerable value space (e.g. a short known list of
   possible names) is vulnerable to a keyed dictionary attack if the pepper ever
   leaks. The pepper's secrecy is load-bearing.
4. **Cross-mesh tokens do not correlate.** Different meshes use different peppers,
   so the same person tokenizes differently across meshes by design.
5. **Reveal is best-effort.** A token whose source raw data is absent on the
   queried node (thin/cache node, evicted RAM entry, foreign origin) is
   unresolvable there.
6. **Free-text masking is heuristic.** Person-name detection over prose depends on
   lexicons/heuristics (or optional local NER); an unusual name in free text can
   in principle escape the heuristic layer, which is why the deterministic regex
   backstop and structured-field tokenization exist as independent nets for
   high-confidence PII.
