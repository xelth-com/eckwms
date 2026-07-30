<!-- machine-first: generated from source audit 2026-07-29; audience=agents -->

# Document-AI Pipeline

## SCOPE

This document specifies the per-node document-AI pipeline: provider auth,
summarization (live and batch), embeddings, the spend observer / circuit breaker,
loop protection, and telemetry. It is descriptive of the shipped code in
`core/src/ai.rs` (provider abstraction + metering seam) and `wms/src/ai/*`
(`summarization.rs`, `summarization_batch.rs`, `embeddings.rs`, `observer.rs`,
`telemetry.rs`, `loop_guard.rs`, and neighbors `doc_classify.rs`,
`translation.rs`). Anchors are `file::symbol`.

All generative and embedding calls target Google's Gemini family. Two auth
backends exist; everything downstream (prompt building, PII masking, completion
write) is shared between them.

---

## 1. Provider abstraction

`core/src/ai.rs::AiAuth` is the one place the backend split lives. Two variants,
selected by `ECK_AI_MODE`:

- **studio** (default, the open-source path): the operator's own Google AI Studio
  key. `AiAuth::Studio { api_key }` from `GEMINI_API_KEY`; requests hit
  `generativelanguage.googleapis.com` with an `x-goog-api-key` header.
- **managed** (paid): a short-lived Vertex AI bearer minted server-side.
  `AiAuth::Vertex { bearer, project, location }`; requests hit
  `aiplatform.googleapis.com` with `Authorization: Bearer …`.

**The open-core seam.** The managed bearer is **minted from an external
commercial token authority** — this repo calls the endpoint but does not
implement it. `AiAuth::resolve` (the live path; `from_env` is the no-mint static
path) mints on demand and caches the bearer process-wide, refreshing it a fixed
margin before its stated expiry. Seam configuration is entirely env:

- `ECK_VERTEX_MINT_URL` — the authority's mint endpoint, authed by
  `ECK_LICENSE_TOKEN`. Returns a bearer + routing (`project`, `location`) + a
  balance snapshot.
- A manual override (pinned bearer via `ECK_VERTEX_BEARER`) short-circuits
  minting.
- `ECK_VERTEX_USAGE_URL` — the metering endpoint (§6).

`ai.rs` keeps a process-wide **mint backoff**: after the authority refuses a mint
(HTTP 402 allowance-exhausted, or an auth failure — a "hard" failure), it stops
asking for an escalating window instead of re-minting every worker cycle. A
suppressed mint fails the AI call for that cycle rather than hammering the
authority. The authority piggybacks a balance snapshot on every mint and usage
response, absorbed process-wide so dashboards reflect the authority's word rather
than local guesses.

In **studio** mode the managed-only paths (batch, mint, metering) are inert:
Studio always falls through to the live `generateContent` path and the metering
seam is a no-op.

---

## 2. Summarization state machine

Worker: `wms/src/ai/summarization.rs::start_summarization_worker`, one tick per
~10 s. State lives in `document.summary_status`. Constants: `MAX_RETRIES = 5`,
`SETTLE_MINS = 10`.

**Eligibility** (`summarization::eligibility_where`, shared by live and batch):
`summary_status = 'pending'` AND source-instance gate AND (no recent write OR
`updated_at < now - SETTLE_MINS`) AND `summary_retries < MAX_RETRIES`. The
**settle window** exists because ingest bumps `updated_at` on every imported
thread; without it the worker would summarize a ticket mid-import repeatedly.

**Backoff.** DB-side exponential backoff: a doc with N retries waits `2^N` minutes
before its next attempt (1, 2, 4, 8, 16), hard-capped at `MAX_RETRIES`.

**Loop guard** (`wms/src/ai/loop_guard.rs::LoopGuard`, in-memory, independent of
DB state): `check_and_record(id)` blocks an entity hit more than `MAX_ATTEMPTS =
3` times within a 60 s window for a `COOLDOWN` of 300 s. This is the last-resort
breaker for the failure mode where a success-path `UPDATE` matches 0 rows (an
id-format regression), so `summary_retries` never advances and the worker would
otherwise loop at full speed. The success path additionally force-advances the
retry counter via an id-insensitive `WHERE` when it matches 0 rows.

**Observer circuit breaker** (`observer.rs` + `telemetry.rs`): a global atomic
`BudgetLevel` (`Normal`/`Warn`/`Throttle`/`Halt`) is evaluated from `ai_telemetry`
token sums against hourly and daily thresholds (`telemetry.rs::evaluate_budget`;
hourly warn/throttle/halt = 3M / 6M / 15M tokens, daily = 20M / 40M / 80M —
highest triggered tier wins). Workers read it lock-free before each call:
`Throttle` sleeps `THROTTLE_DELAY_SECS = 60` between calls; `Halt` stops all AI
work.

**Sticky pause (`paused_by_observer`).** On a *critical* anomaly the observer
auto-mitigates **only** when (a) workers made zero progress in 15 min AND (b) at
least 10 *stale* pendings exist (`updated_at` older than 30 min) — a healthy
draining backlog is never paused. Mitigation flips those stale pendings to
`paused_by_observer`. This state is **deliberately not requeued** by the periodic
retry reset: the pause is sticky and requires an explicit operator/admin action
to resume, so a runaway cannot immediately re-arm itself.

**Terminal / recovery states.** `error` and `skipped` are requeued to `pending`
by `reset_retryable` while `summary_retries < MAX_RETRIES`; past the cap the doc
becomes `failed` (permanent, not retried). `enriching` is a within-run ingest
state (threads still landing); the ingest scheduler flips `enriching → pending`
when a ticket finalizes, and a rescue pass requeues any doc stuck `enriching`
after ~1 h of write silence (a crashed run). `completed` re-arms to `pending`
only when the source content or the policy seed hash changes.

### STATE MACHINE

`summary_status` transition table (`document`):

| From | Event | To | Anchor |
|------|-------|----|--------|
| (ingest) | threads landing | `enriching` | ingest scheduler |
| `enriching` | ticket finalized | `pending` | ingest scheduler |
| `enriching` | stale >~1h (crashed run) | `pending` | rescue pass |
| `pending` | eligible + live summarize OK | `completed` | `process_pending` → `write_summary_completion` |
| `pending` | empty source text | `skipped` (retry++) | `process_pending` |
| `pending` | summarize call error | `error` (retry++) | `process_pending` |
| `pending` | claimed into batch (managed) | `batched` | `summarization_batch::batch_tick` |
| `batched` | batch output parsed OK | `completed` | `finalize_batch_job` |
| `batched` | job failure / revert | `pending` (retry per cause) | `revert_claimed` |
| `batched` | orphaned >48h (lost job row) | `pending` | orphan guard |
| `error`/`skipped` | retries < `MAX_RETRIES` | `pending` | `reset_retryable` |
| `error`/`skipped` | retries ≥ `MAX_RETRIES` | `failed` (terminal) | retry cap |
| `pending` | critical anomaly + no progress + stale | `paused_by_observer` (sticky) | `observer` auto-mitigation |
| `completed` | source/seed-hash change | `pending` | re-arm (seed marker) |

(The embedding worker uses a parallel `embedding_status` with the same
`pending → complete/error` shape and the same `paused_by_observer` pause; see §4.)

---

## 3. Batch mode

`wms/src/ai/summarization_batch.rs` runs **inside** the same summarization worker
loop — one `batch_tick` per 10 s cycle, no extra task, so there are no cross-task
DB races. Managed/Vertex only; **studio always falls through to live**.

**Eligibility gate.** Submit a batch only when `ECK_SUMMARY_BATCH=1`, no job is
already in flight, and the eligible pile (same `eligibility_where` as live,
probed by `min_eligible_reached`) is at least `ECK_SUMMARY_BATCH_MIN` (default
10). Batch size is capped at `ECK_SUMMARY_BATCH_MAX` (default 200).

**Claim semantics.** Selected docs are flipped `pending → batched`
(`updated_at = now`), and only rows the `UPDATE` confirms are kept (a peer or the
live path may have claimed one first). The `updated_at` bump makes the 48 h orphan
guard measure time-since-claim.

**GCS JSONL round-trip.** Batch prediction requires GCS in/out. One input object
`.../input.jsonl` is uploaded; a Vertex `batchPredictionJob` is created with
`instancesFormat/predictionsFormat = jsonl`; predictions are read back from the
job's output prefix. The bucket is env-configurable (a code default exists).

**Keying — sha256 of the canonical request.** Output rows come back in a
**different order** than submitted and carry no doc id (the document id is never
placed in the prompt). Vertex echoes the full submitted `request` verbatim
(only re-ordering object keys), so each row is keyed by the **sha256 of the
canonical (recursively key-sorted) serialization of its `request`**
(`batch_sha256_key` over `canonical_json`), computed identically at submit time
(stored in the job row's `docs[]`) and at parse time over the echoed request.
Output order is therefore not relied upon.

**Request divergences from the live payload** (all deliberate, documented in the
module header):
(a) `contents[].role = "user"` set **explicitly** (the live path injects it in
`generate_content_raw`, which batch rows bypass);
(b) `generationConfig.thinkingConfig.thinkingBudget = 0` set explicitly (without
it the thinking models starve the JSON answer);
(c) the `googleSearch` grounding tool is **omitted** (grounding in a batch job is
unproven) — batch summaries are not address-grounded, so the hourly geo sweep
geocodes those tickets separately.
Everything else (system prompt, masked user text, temperature, maxOutputTokens)
is byte-identical to live via the shared `summarization` helpers, so both paths
produce equivalent summaries and share the completion write.

**Crash-safety order.** `submit_batch` does Select → build → claim → **create the
job row (`state='submitting'`) and GCS object layout FIRST** → upload → submit.
A crash before the job exists leaves docs reverted (no retry penalty — infra, not
the doc's fault) and stray GCS objects cleaned. Backstops: a `submitting` row
with no `job_name` after `SUBMIT_STALE_AGE` (10 m) is treated as a crashed submit
and torn down; docs stranded in `batched` past `ORPHAN_MAX_AGE` (48 h) are
requeued (`orphan_guard`).

**Idempotent completion.** The job row carries `usage_reported: bool`. If a prior
run already metered (`usage_reported = true`) but crashed before marking the job
`done`, finalization does **not** re-summarize or re-bill — it just finishes.

**Aggregated metering.** All rows of one completed job report **one** aggregated
usage figure to the authority, tagged with the model name suffixed `-batch`
(`report_managed_usage(&format!("{model}-batch"), "summarize_batch", &summed)`),
then `usage_reported` is set true.

---

## 4. Embeddings

`wms/src/ai/embeddings.rs`. Vectors are content that must converge across the
mesh without every node recomputing them.

- **One-author model.** A worker only embeds rows whose vector is **absent**
  (`WHERE embedding_status = 'pending' AND embedding IS NONE`). On a successful
  write it **bumps this node's `_vclock`**
  (`conflict::bump_local_vclock_by_leaf`), so its vector strictly dominates a
  peer's vector-less copy and peers adopt it via normal conflict resolution.
- **Consumer nodes.** Set `ECK_EMBED_WORKER=0` on every non-AI node; they never
  embed, they receive the authored vector over the mesh. `embedding` and
  `embedding_model` are **not** in `IGNORED_FIELDS` (they are content); the
  per-node `embedding_status`/`retries`/`error` are, so a consumer legitimately
  stays `pending` while the vector arrives.
- **Re-embed signal.** Re-embedding is requested fleet-wide by **nulling the
  vector**. Because `embedding` participates in the content hash, a nulled vector
  is a synced, hash-visible signal: whichever worker meets the missing vector
  first re-authors it. This is how a policy flip (§`30-pprl.md` §5) re-embeds a
  corpus without a side channel.

Query embeddings are anonymized before embedding to match the stored (masked)
vectors; vectors are 768-dim.

---

## 5. Adjacent stages

- **Classification** (`wms/src/ai/doc_classify.rs`): a layer-2 classifier writes a
  nested `doc_class` with a float `confidence`. That float is why the content-hash
  canonicalizer rounds through `f32` at any depth (§`20-mesh-sync.md` §1.1) — the
  classifier's confidence picks up cross-node ULP residue otherwise.
- **Translation** (`wms/src/ai/translation.rs`): on-demand machine translation of
  user-facing content (e.g. summaries) into a viewer's language. Rows use a
  deterministic id derived from `(source, field, lang)` and are content-hashed
  over their business fields, so a translation produced on one node converges to
  peers instead of each node re-calling the model; translation claims + results
  are on the cross-NAT relay-sync subset so work-dedup holds off-LAN too.

---

## 6. Telemetry & the metering seam

**Local telemetry.** Every AI call logs to the `ai_telemetry` table
(`telemetry.rs::log_telemetry`). Row shape:

```
ai_telemetry {
  timestamp:         datetime,   // time::now()
  module:            string,     // "summarization" | "embeddings" | ...
  model:             string,     // model id (batch rows tagged "<model>-batch")
  entity_id:         string,
  prompt_tokens:     int,        // usageMetadata.promptTokenCount
  candidates_tokens: int,        // usageMetadata.candidatesTokenCount
  total_tokens:      int,        // usageMetadata.totalTokenCount
}
```

This table is the sole input to the observer's budget evaluation (§2), summed
over 1 h / 24 h windows.

**The usage-report seam.** `core/src/ai.rs::report_managed_usage(model, kind,
usage)` is a **fire-and-forget** `tokio::spawn`ed POST to `ECK_VERTEX_USAGE_URL`
(authed by `ECK_LICENSE_TOKEN`). It is a **no-op** when either env is unset —
i.e. outside managed mode, or on a build with no authority configured. It is
called by both the in-process live meter (`AiAuth::report_usage`, per
`generateContent`) and the out-of-band batch meter (one aggregated report per
job, model tagged `-batch`). The authority answers every report with the current
mesh balance, which is absorbed process-wide so the dashboard gauge refreshes on
every AI call, not just on the ~hourly mint.

---

## STATE MACHINE (summary)

- Live and batch summarization share one eligibility predicate and one completion
  write; batch is a managed-only optimization that claims into `batched` and back.
- The pipeline is self-healing in place: retryable states requeue under a retry
  cap, a within-run state is rescued, orphaned batch claims are requeued, and the
  in-memory loop guard plus the DB retry counter are independent breakers.
- The observer is the outer breaker: budget tiers throttle then halt all AI work,
  and a sticky `paused_by_observer` state prevents a detected runaway from
  re-arming itself without operator action.
- Terminal `failed` (retry cap) is the only state the pipeline does not
  auto-recover from.
