//! Deterministic extraction of the UNIQUE body of one support-thread message —
//! the text the sender actually typed, with the quoted history of every earlier
//! message removed. NO LLM: a fixed strategy ladder tuned against the real Zoho
//! Desk / Outlook / Gmail markers that live in `document_raw.payload.content`.
//!
//! Storage context: a `document_raw:<thread_id>` row (local only) holds the full
//! Zoho thread payload — `content` (HTML body, median ~17 KB, >90 % of which is
//! quoted history), plus Zoho's own de-quoted plain-text opening in `summary`
//! (~183 chars, "…"-truncated, empty on ~10 % of rows). This module turns that
//! HTML into just the fresh reply so an agent can read what the customer said
//! ("did they sign the Kostenvoranschlag?") without wading through the chain.
//!
//! The public entry point is [`extract_unique_body`]; it is pure and unit-tested
//! below against the concrete Zoho/Outlook/Gmail shapes seen in production.

/// Outcome of [`extract_unique_body`].
///
/// * `text` — the extracted unique body (never empty when the input was not).
/// * `method` — which ladder rung produced `text`, for observability.
/// * `truncated_quote_bytes` — how many bytes of plain text were dropped as
///   quoted history (full plain-text length − returned length).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedBody {
    pub text: String,
    pub method: &'static str,
    pub truncated_quote_bytes: usize,
}

/// HTML-structural quote-boundary markers, searched case-insensitively on the
/// (script/style-stripped) HTML. First match wins; whichever sits earliest in
/// the document is the true boundary. All markers are ASCII, so byte offsets
/// into the lowercased copy map 1:1 onto the original.
const HTML_QUOTE_MARKERS: &[(&str, &str)] = &[
    // Zoho Desk's own explicit reply separator, emitted right before the quote.
    ("beforequote:::", "quote_zoho"),
    // Outlook's send-point anchor + reply/forward header container.
    ("appendonsend", "quote_header"),
    ("divrplyfwdmsg", "quote_header"),
    // Gmail / Thunderbird quote wrappers.
    ("gmail_quote", "quote_gmail"),
    ("moz-cite-prefix", "quote_gmail"),
    // The quoted-history element itself (last resort — always present, but the
    // markers above usually sit earlier and trim the attribution line too).
    ("<blockquote", "quote_blockquote"),
];

/// Strip tags from HTML → plain text, decoding entities and dropping
/// `<script>`/`<style>` bodies. Reuses the crate's shared tag-stripper
/// (`ai::summarization::strip_html`) as the tag primitive so the fleet keeps one
/// definition of "which tags become line breaks"; the script/style pre-pass and
/// entity post-pass are the task-specific additions the shared helper lacks.
///
/// Exposed so a caller can measure the FULL message size (e.g. `full_text_chars`
/// in the `ticket_thread` MCP tool) with the exact same normalization the
/// extractor sees.
pub fn html_to_text(content_html: &str) -> String {
    let cleaned = drop_scripts_styles(content_html);
    decode_entities(&crate::ai::summarization::strip_html(&cleaned))
}

/// Extract the unique (non-quoted) body of one message.
///
/// * `content_html` — the raw `payload.content` HTML.
/// * `zoho_summary` — `payload.summary` if present (Zoho's de-quoted opening);
///   the highest-confidence signal, used to validate the quote cut.
/// * `previous_bodies` — the already-extracted texts of the EARLIER messages in
///   the same thread, for the marker-less subtraction fallback.
pub fn extract_unique_body(
    content_html: &str,
    zoho_summary: Option<&str>,
    previous_bodies: &[&str],
) -> ExtractedBody {
    // ── Step 1: HTML → plain text ──
    let cleaned_html = drop_scripts_styles(content_html);
    let full_plain = decode_entities(&crate::ai::summarization::strip_html(&cleaned_html));
    let full_len = full_plain.len();

    // Non-empty input must never yield an empty body — if there is literally no
    // text, fall straight to the summary.
    if full_plain.trim().is_empty() {
        if let Some(sum) = clean_summary(zoho_summary) {
            return ExtractedBody { text: sum, method: "fallback_summary", truncated_quote_bytes: 0 };
        }
        return ExtractedBody { text: String::new(), method: "empty", truncated_quote_bytes: 0 };
    }

    // ── Step 2: quote-boundary cut ──
    // (a) HTML-structural cut (blockquote / Zoho / Outlook / Gmail wrappers).
    let html_lower = cleaned_html.to_ascii_lowercase();
    let (html_head, html_method) = match earliest_marker(&html_lower, HTML_QUOTE_MARKERS) {
        Some((off, name)) => (&cleaned_html[..off], Some(name)),
        None => (cleaned_html.as_str(), None),
    };
    let head_text = decode_entities(&crate::ai::summarization::strip_html(html_head));
    // (b) text-level cut on the head (residual attribution lines the structural
    //     cut leaves behind, e.g. Zoho's "---- an … hat geschrieben ----").
    let (mut candidate, mut method): (String, &'static str) = match cut_text_quote(&head_text, 0) {
        Some((off, name)) => (head_text[..off].trim().to_string(), name),
        None => (head_text.trim().to_string(), html_method.unwrap_or("html_only")),
    };

    // ── Step 3: anchor on / validate against the Zoho summary ──
    // If the summary's opening sentence is NOT inside the step-2 result, the cut
    // fired too early (a false marker inside the real reply); re-derive from the
    // full text anchored on the summary — the highest-confidence signal.
    if let Some(anchor) = summary_anchor(zoho_summary) {
        let cand_norm = normalize_ws(&candidate).to_lowercase();
        if !cand_norm.contains(&anchor) {
            if let Some(anchored) = anchor_cut(&full_plain, &anchor) {
                candidate = anchored;
                method = "summary_anchor";
            }
        }
    }

    // ── Step 4: subtract previously-seen messages (marker-less guard) ──
    if let Some(trimmed) = subtract_previous(&candidate, previous_bodies) {
        candidate = trimmed;
        if method == "html_only" {
            method = "prev_subtract";
        }
    }

    // ── Step 5: trivial signature trim (only the RFC "-- " delimiter) ──
    candidate = trim_signature(&candidate);

    // ── Never-empty fallback ──
    let candidate = candidate.trim().to_string();
    if candidate.is_empty() {
        if let Some(sum) = clean_summary(zoho_summary) {
            return ExtractedBody {
                text: sum,
                method: "fallback_summary",
                truncated_quote_bytes: full_len,
            };
        }
        let head = safe_prefix(&full_plain, 2000);
        let trunc = full_len.saturating_sub(head.len());
        return ExtractedBody { text: head, method: "fallback_head", truncated_quote_bytes: trunc };
    }

    let trunc = full_len.saturating_sub(candidate.len());
    ExtractedBody { text: candidate, method, truncated_quote_bytes: trunc }
}

// ─── HTML helpers ───────────────────────────────────────────────────────────

/// Remove `<script>…</script>` and `<style>…</style>` including their bodies
/// (case-insensitive). Tag delimiters are ASCII, so slicing on the byte offsets
/// found in the lowercased copy is char-boundary-safe.
fn drop_scripts_styles(html: &str) -> String {
    if !html.contains('<') {
        return html.to_string();
    }
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut pos = 0usize;
    while pos < html.len() {
        let next = ["<script", "<style"]
            .iter()
            .filter_map(|t| lower[pos..].find(t).map(|p| (pos + p, *t)))
            .min_by_key(|(p, _)| *p);
        match next {
            None => {
                out.push_str(&html[pos..]);
                break;
            }
            Some((start, tag)) => {
                out.push_str(&html[pos..start]);
                let close = if tag == "<script" { "</script" } else { "</style" };
                match lower[start..].find(close) {
                    Some(cp) => {
                        let after = start + cp;
                        match html[after..].find('>') {
                            Some(gt) => pos = after + gt + 1,
                            None => break, // malformed — drop the rest
                        }
                    }
                    None => break, // unterminated — drop the rest
                }
            }
        }
    }
    out
}

/// Decode the HTML entities that appear in this data set: `&nbsp;`, `&amp;`,
/// `&lt;`, `&gt;`, `&quot;`, `&apos;`, numeric `&#NN;` / `&#xHH;`, and the German
/// named set. Unknown entities are passed through verbatim.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';') {
            if semi > 0 && semi <= 10 {
                if let Some(dec) = decode_one_entity(&after[..semi]) {
                    out.push_str(&dec);
                    rest = &after[semi + 1..];
                    continue;
                }
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn decode_one_entity(name: &str) -> Option<String> {
    let c = match name {
        "nbsp" => ' ',
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "auml" => 'ä',
        "ouml" => 'ö',
        "uuml" => 'ü',
        "Auml" => 'Ä',
        "Ouml" => 'Ö',
        "Uuml" => 'Ü',
        "szlig" => 'ß',
        "eacute" => 'é',
        "egrave" => 'è',
        "agrave" => 'à',
        "ccedil" => 'ç',
        "euro" => '€',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "laquo" => '«',
        "raquo" => '»',
        _ => {
            let code = if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                name.strip_prefix('#').and_then(|d| d.parse::<u32>().ok())
            };
            return code.and_then(char::from_u32).map(|c| c.to_string());
        }
    };
    Some(c.to_string())
}

// ─── Text-level quote markers ────────────────────────────────────────────────

/// Earliest byte offset (and method label) of any `(marker, method)` in `hay`.
fn earliest_marker(hay: &str, markers: &[(&str, &'static str)]) -> Option<(usize, &'static str)> {
    markers
        .iter()
        .filter_map(|(m, name)| hay.find(m).map(|p| (p, *name)))
        .min_by_key(|(p, _)| *p)
}

/// Scan the plain text line by line for a quote-start marker at or after
/// `min_off`; return the byte offset of the START of the matching line and the
/// method label. Covers German + English Zoho/Outlook/Gmail attribution shapes
/// and `>`-quoted blocks.
fn cut_text_quote(text: &str, min_off: usize) -> Option<(usize, &'static str)> {
    let lines: Vec<(usize, &str)> = line_offsets(text);
    let mut best: Option<(usize, &'static str)> = None;
    let mut consider = |off: usize, name: &'static str, best: &mut Option<(usize, &'static str)>| {
        if off >= min_off && best.map_or(true, |(b, _)| off < b) {
            *best = Some((off, name));
        }
    };

    for (idx, (off, raw)) in lines.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let low = line.to_lowercase();

        // German attribution shapes:
        //  * Zoho "---- an <date> … hat geschrieben ----" (corroborated by dashes/
        //    email/year so a bare "… hat geschrieben …" prose line isn't a boundary),
        //  * a line ENDING in "schrieb:" (Apple/BlueMail "<name> schrieb:"),
        //  * "Am <date> … schrieb …:" (Gmail-DE).
        let trimmed_low = low.trim_end();
        if (low.contains("hat geschrieben")
            && (line.contains("----") || line.contains('@') || contains_year(line)))
            || trimmed_low.ends_with("schrieb:")
            || (starts_with_ci(&low, "am ") && low.contains("schrieb") && line.contains(':'))
        {
            consider(*off, "quote_attribution_de", &mut best);
        }

        // English attribution: a line ending in "wrote:" ("<name> wrote:",
        // "On <date> … wrote:").
        if trimmed_low.ends_with("wrote:") || (starts_with_ci(&low, "on ") && low.contains("wrote:")) {
            consider(*off, "quote_attribution_en", &mut best);
        }

        // Explicit separators, incl. the hyphenated German desktop form
        // "-----Original-Nachricht-----" (subject/date-first header follows).
        if (low.contains("original") && low.contains("nachricht"))
            || low.contains("ursprüngliche nachricht")
            || low.contains("original message")
        {
            consider(*off, "quote_original", &mut best);
        }

        // Reply/forward header block — "Von:"/"From:" heading the rest of the
        // cluster (Gesendet/Sent/An/To/Betreff/Subject), whether those fields
        // land on the FOLLOWING lines (web Outlook, <br>-separated) or collapse
        // onto the SAME line (German desktop clients emit no line breaks).
        if (starts_with_ci(&low, "von:") || starts_with_ci(&low, "from:"))
            && (header_cluster_follows(&lines, idx) || same_line_header(&low))
        {
            consider(*off, "quote_header", &mut best);
        }

        // A run of `>`-quoted text.
        if line.starts_with('>') && line.len() > 1 {
            consider(*off, "quote_gt", &mut best);
        }
    }
    best
}

/// Same-line header form: a `Von:`/`From:` line that also carries another header
/// field inline (`Gesendet:`/`Sent:`/`Betreff:`/`Subject:`). High-precision — a
/// prose line never opens with `Von:` and also contains `Gesendet:`.
fn same_line_header(lowercased_line: &str) -> bool {
    ["gesendet:", "sent:", "betreff:", "subject:"]
        .iter()
        .any(|f| lowercased_line.contains(f))
}

/// Does a `Von:`/`From:` line at `idx` begin a real header block? True if one of
/// the next few non-empty lines is another header field.
fn header_cluster_follows(lines: &[(usize, &str)], idx: usize) -> bool {
    let mut seen = 0;
    for (_, raw) in lines.iter().skip(idx + 1) {
        let l = raw.trim();
        if l.is_empty() {
            continue;
        }
        let low = l.to_lowercase();
        if ["gesendet:", "sent:", "an:", "to:", "cc:", "betreff:", "subject:"]
            .iter()
            .any(|p| starts_with_ci(&low, p))
        {
            return true;
        }
        seen += 1;
        if seen >= 4 {
            break;
        }
    }
    false
}

// ─── Summary anchor ──────────────────────────────────────────────────────────

/// Normalize the Zoho summary into a short anchor = its first sentence
/// (terminated by `.`/`!`/`?`, capped at ~90 chars). Kept SHORT on purpose:
/// Zoho's summary bleeds into the quoted header once the reply is a few words,
/// so a first-sentence anchor stays inside the real reply. Returns
/// `None` when there is no usable summary.
fn summary_anchor(summary: Option<&str>) -> Option<String> {
    let s = clean_summary(summary)?;
    let norm = normalize_ws(&s).to_lowercase();
    if norm.is_empty() {
        return None;
    }
    let mut end = norm.len();
    for (i, c) in norm.char_indices() {
        if matches!(c, '.' | '!' | '?') {
            end = i + c.len_utf8();
            break;
        }
    }
    let mut anchor = norm[..end].trim().to_string();
    if anchor.chars().count() > 90 {
        anchor = safe_prefix(&anchor, 90).trim().to_string();
    }
    if anchor.len() < 3 {
        return None;
    }
    Some(anchor)
}

/// Anchor-based cut: locate `anchor` (whitespace-flexible) in the full plain
/// text and return everything from there to the first quote marker that follows
/// it. Bounds the result at 8 KB. `None` if the anchor cannot be located.
fn anchor_cut(full_plain: &str, anchor: &str) -> Option<String> {
    let (hay, offs) = norm_chars(full_plain);
    let needle: Vec<char> = anchor.chars().collect();
    let k = find_subslice(&hay, &needle)?;
    let astart = offs[k];
    let aend_idx = k + needle.len();
    let aend = offs.get(aend_idx).copied().unwrap_or(full_plain.len());
    let cut = cut_text_quote(full_plain, aend)
        .map(|(o, _)| o)
        .unwrap_or(full_plain.len());
    let body = full_plain.get(astart..cut).unwrap_or("").trim();
    if body.is_empty() {
        return None;
    }
    Some(safe_prefix(body, 8000))
}

// ─── Previous-message subtraction ────────────────────────────────────────────

/// Strip the longest trailing block of `candidate` that also appears (normalized
/// whitespace) in any `previous_bodies` entry. Returns the trimmed head, or
/// `None` when nothing was removed / removal would empty the body.
fn subtract_previous(candidate: &str, previous_bodies: &[&str]) -> Option<String> {
    let lines: Vec<&str> = candidate.lines().collect();
    if lines.len() < 2 {
        return None;
    }
    let prev_norms: Vec<String> = previous_bodies
        .iter()
        .map(|p| normalize_ws(p).to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if prev_norms.is_empty() {
        return None;
    }
    // Smallest `start` whose suffix is a duplicate block == longest such suffix.
    for start in 1..lines.len() {
        let block_norm = normalize_ws(&lines[start..].join("\n")).to_lowercase();
        if block_norm.len() >= 25 && prev_norms.iter().any(|p| p.contains(&block_norm)) {
            let head = lines[..start].join("\n").trim().to_string();
            return if head.is_empty() { None } else { Some(head) };
        }
    }
    None
}

// ─── Signature trim ──────────────────────────────────────────────────────────

/// Cut at the RFC-3676 signature delimiter line (`-- `, which normalizes to
/// `--`) or at the corporate confidentiality notice that every outbound service
/// mail carries (~1.3 KB of boilerplate, measured on ~14 % of real messages).
/// Conservative by design: only these exact shapes, never a guess. Nothing of
/// substance is ever written *below* a legal disclaimer or its `****` rule, so
/// the cut is safe; greetings and signature blocks are deliberately LEFT IN —
/// they are short and often carry the sender's name.
fn trim_signature(text: &str) -> String {
    let mut off = 0usize;
    for piece in text.split_inclusive('\n') {
        let content = piece.trim_end_matches('\n');
        let trimmed = content.trim();
        if off > 0 && (trimmed == "--" || is_boilerplate_boundary(trimmed)) {
            return text[..off].trim_end().to_string();
        }
        off += piece.len();
    }
    text.to_string()
}

/// A line that opens the legal-disclaimer block: either the long `*` rule that
/// fences it, or the German/English confidentiality sentence itself.
fn is_boilerplate_boundary(line: &str) -> bool {
    if line.len() >= 20 && line.chars().all(|c| c == '*') {
        return true;
    }
    let low = line.to_lowercase();
    ["diese e-mail enthält vertrauliche", "diese email enthält vertrauliche"]
        .iter()
        .any(|p| low.starts_with(p))
        || low.starts_with("this e-mail may contain confidential")
        || low.starts_with("this email may contain confidential")
}

// ─── Small string utilities ──────────────────────────────────────────────────

fn clean_summary(summary: Option<&str>) -> Option<String> {
    let s = summary?.trim();
    let s = s.strip_suffix("...").or_else(|| s.strip_suffix('…')).unwrap_or(s).trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn starts_with_ci(lowercased_haystack: &str, lowercase_prefix: &str) -> bool {
    lowercased_haystack.starts_with(lowercase_prefix)
}

fn contains_year(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 4 {
        return false;
    }
    // Any "20xx" run (2000-2099) — the year in a quoted date line.
    for w in bytes.windows(4) {
        if w[0] == b'2' && w[1] == b'0' && w[2].is_ascii_digit() && w[3].is_ascii_digit() {
            return true;
        }
    }
    false
}

/// Take the first `max_bytes` bytes, backing up to a UTF-8 char boundary.
fn safe_prefix(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// `(byte_offset_of_line_start, line_without_trailing_newline)` for each line.
fn line_offsets(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    for piece in text.split_inclusive('\n') {
        out.push((off, piece.trim_end_matches('\n')));
        off += piece.len();
    }
    out
}

/// Lowercase + collapse whitespace to single spaces, returning the normalized
/// chars alongside, for each output char, the ORIGINAL byte offset it came from.
/// Lets a normalized substring match be mapped back to a cut point in the raw
/// text.
fn norm_chars(s: &str) -> (Vec<char>, Vec<usize>) {
    let mut chars = Vec::new();
    let mut offs = Vec::new();
    let mut prev_ws = false;
    for (b, c) in s.char_indices() {
        if c.is_whitespace() {
            if !prev_ws {
                chars.push(' ');
                offs.push(b);
                prev_ws = true;
            }
        } else {
            for lc in c.to_lowercase() {
                chars.push(lc);
                offs.push(b);
            }
            prev_ws = false;
        }
    }
    (chars, offs)
}

fn find_subslice(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: fixtures mirror the SHAPES observed in production, with invented
    // names/addresses — no real customer PII appears in this file.

    #[test]
    fn outlook_german_reply_with_von_header() {
        // Outlook: reply divs, appendonsend anchor, hr, divRplyFwdMsg header.
        let html = r#"<meta/><div dir="ltr" class="elementToProof">
Können wir das Gerät selbst öffnen?</div>
<div class="elementToProof"><br/></div>
<div class="elementToProof">Viele Grüße</div>
<div id="x_1_appendonsend"></div>
<hr style="display: inline-block; width: 98%"/>
<div id="x_1_divRplyFwdMsg" dir="ltr"><font><b>Von:</b> Max Beispiel &lt;max@example.test&gt;<br/>
<b>Gesendet:</b> Montag, 17. April 2023 15:25<br/>
<b>An:</b> Support &lt;support@example.test&gt;<br/>
<b>Betreff:</b> Re: Anfrage</font></div>
<blockquote><div>alter zitierter Text hier</div></blockquote>"#;
        let out = extract_unique_body(html, None, &[]);
        assert!(out.text.contains("Können wir das Gerät selbst öffnen?"));
        assert!(out.text.contains("Viele Grüße"));
        assert!(!out.text.contains("zitierter Text"));
        assert!(!out.text.contains("Von:"));
        assert_eq!(out.method, "quote_header");
        assert!(out.truncated_quote_bytes > 0);
    }

    #[test]
    fn zoho_outbound_hat_geschrieben_and_blockquote() {
        let html = r#"<meta/><div><div>Sehr geehrter Herr Beispiel,<br/></div>
<div><br/></div><div>das Ersatzteil ist bestellt.<br/></div>
<div>Sportliche Grüße<br/></div><div>Andreas K.</div>
<div><br/>---- an Mon, 17 Apr 2023 14:19:40 +0200 <b>&quot;Max Beispiel&quot;&lt;max@example.test&gt;</b> hat geschrieben ---- <br/></div>
<div title="beforequote:::"></div>
<div><blockquote><div>frühere Nachricht, bitte ignorieren</div></blockquote></div></div>"#;
        let out = extract_unique_body(html, None, &[]);
        assert!(out.text.contains("das Ersatzteil ist bestellt."));
        assert!(out.text.contains("Andreas K."));
        assert!(!out.text.contains("hat geschrieben"));
        assert!(!out.text.contains("frühere Nachricht"));
        assert_eq!(out.method, "quote_attribution_de");
    }

    #[test]
    fn gmail_english_on_wrote() {
        let html = "<div>Thanks, that fixed it!</div>\
<blockquote class=\"gmail_quote\"><div>On Mon, Apr 17, 2023 at 3:25 PM Support &lt;support@example.test&gt; wrote:</div>\
<div>please try a factory reset</div></blockquote>";
        let out = extract_unique_body(html, None, &[]);
        assert!(out.text.contains("Thanks, that fixed it!"));
        assert!(!out.text.contains("factory reset"));
        assert!(!out.text.to_lowercase().contains("wrote:"));
        // The `<blockquote` element start sits earliest, so it is the winning
        // structural boundary (the `gmail_quote` class is a few bytes later).
        assert_eq!(out.method, "quote_blockquote");
    }

    #[test]
    fn plain_gt_quoted_block() {
        let html = "<div>Nein, das passt so.</div><div>&gt; war Ihre vorherige Frage</div><div>&gt; mit Details</div>";
        let out = extract_unique_body(html, None, &[]);
        assert_eq!(out.text, "Nein, das passt so.");
        assert_eq!(out.method, "quote_gt");
    }

    #[test]
    fn original_message_separator() {
        let html = "<div>Hier die gewünschte Info.</div>\
<div>-----Ursprüngliche Nachricht-----</div>\
<div>Von: jemand</div><div>alter Inhalt</div>";
        let out = extract_unique_body(html, None, &[]);
        assert!(out.text.contains("Hier die gewünschte Info."));
        assert!(!out.text.contains("alter Inhalt"));
        assert_eq!(out.method, "quote_original");
    }

    #[test]
    fn signature_delimiter_trimmed() {
        let html = "<div>Die Lieferung ist angekommen.</div><div>--</div><div>Max Beispiel</div><div>Firma GmbH</div>";
        let out = extract_unique_body(html, None, &[]);
        assert_eq!(out.text, "Die Lieferung ist angekommen.");
    }

    #[test]
    fn legal_disclaimer_block_trimmed() {
        // Every outbound service mail ends with an asterisk-fenced confidentiality
        // notice (~1.3 KB). It survives the quote cut (it belongs to THIS message)
        // and is pure noise for a reader, so the signature trim takes it out.
        let stars = "*".repeat(80);
        let html = format!(
            "<div>Anbei der Kostenvoranschlag.</div><div>Mit freundlichen Grüßen</div>\
<div>Ihr Support-Team</div><div>{stars}</div>\
<div>Diese E-Mail enthält vertrauliche und/oder rechtlich geschützte Informationen. \
Wenn Sie nicht der richtige Adressat sind, vernichten Sie diese Mail.</div>"
        );
        let out = extract_unique_body(&html, None, &[]);
        assert!(out.text.contains("Anbei der Kostenvoranschlag."));
        // Greeting/signature stay — only the legal block goes.
        assert!(out.text.contains("Ihr Support-Team"));
        assert!(!out.text.contains("vertrauliche"));
        assert!(!out.text.contains('*'));
    }

    #[test]
    fn english_disclaimer_without_star_rule_trimmed() {
        let html = "<div>Please find the quote attached.</div>\
<div>This e-mail may contain confidential and/or privileged information. \
Any unauthorized copying is strictly forbidden.</div>";
        let out = extract_unique_body(html, None, &[]);
        assert_eq!(out.text, "Please find the quote attached.");
    }

    #[test]
    fn summary_anchor_rescues_early_cut() {
        // Bottom-posted reply: the customer's answer sits BELOW a top-quoted
        // block. The `>`-quote marker fires at offset 0 and empties the step-2
        // candidate; the summary (Zoho's de-quoted opening = the real answer)
        // anchors the correct body further down.
        let html = "<div>&gt; Haben Sie das Gerät erhalten?</div>\
<div>&gt; Bitte um Rückmeldung.</div>\
<div>Ja, alles angekommen, vielen Dank.</div>";
        let summary = Some("Ja, alles angekommen, vielen Dank...");
        let out = extract_unique_body(html, summary, &[]);
        assert_eq!(out.text, "Ja, alles angekommen, vielen Dank.");
        assert!(!out.text.contains("Haben Sie das Gerät erhalten"));
        assert_eq!(out.method, "summary_anchor");
    }

    #[test]
    fn previous_body_subtraction_without_markers() {
        // No quote markers at all — the tail duplicates an earlier message.
        let prev = "Sehr geehrter Kunde, bitte senden Sie das Gerät ein. Wir prüfen es dann umgehend.";
        let html = "<div>Alles klar, ich schicke es morgen los.</div>\
<div>Sehr geehrter Kunde, bitte senden Sie das Gerät ein. Wir prüfen es dann umgehend.</div>";
        let out = extract_unique_body(html, None, &[prev]);
        assert!(out.text.contains("Alles klar, ich schicke es morgen los."));
        assert!(!out.text.contains("bitte senden Sie das Gerät ein"));
        assert_eq!(out.method, "prev_subtract");
    }

    #[test]
    fn never_empty_falls_back_to_summary() {
        // Content is ONLY a quoted block → step 2 empties it → summary fallback.
        let html = "<div title=\"beforequote:::\"></div><blockquote><div>nur zitierter Text</div></blockquote>";
        let summary = Some("Passt, wir organisieren die Abholung...");
        let out = extract_unique_body(html, summary, &[]);
        assert_eq!(out.text, "Passt, wir organisieren die Abholung");
        assert_eq!(out.method, "fallback_summary");
    }

    #[test]
    fn entities_and_scripts_handled() {
        let html = "<style>.x{color:red}</style><script>var a='<blockquote>';</script>\
<div>Preis &amp; Versand: &lt;b&gt; 5&nbsp;&euro; &#38; mehr f&uuml;r Gr&ouml;&szlig;e</div>";
        let out = extract_unique_body(html, None, &[]);
        assert!(out.text.contains("Preis & Versand"));
        assert!(out.text.contains("5 €"));
        assert!(out.text.contains("für Größe"));
        assert!(!out.text.contains("color:red"));
        assert!(!out.text.contains("var a"));
    }

    #[test]
    fn short_reply_survives() {
        // A legitimately tiny reply above a big quote must not be over-cut.
        let html = "<div>Danke!</div><div id=\"x_1_appendonsend\"></div><hr/>\
<div id=\"x_1_divRplyFwdMsg\"><b>Von:</b> Support &lt;support@example.test&gt;<br/>\
<b>Gesendet:</b> Dienstag<br/><b>An:</b> Max<br/><b>Betreff:</b> Re</div>\
<blockquote><div>langer alter Verlauf …</div></blockquote>";
        let summary = Some("Danke! Von: Support <support@example.test> Gesendet: Dienstag...");
        let out = extract_unique_body(html, summary, &[]);
        assert_eq!(out.text, "Danke!");
    }

    #[test]
    fn empty_input_yields_empty() {
        let out = extract_unique_body("", None, &[]);
        assert_eq!(out.text, "");
        assert_eq!(out.method, "empty");
    }

    #[test]
    fn html_to_text_measures_full_message() {
        let html = "<div>Zeile eins</div><blockquote><div>Zeile zwei zitiert</div></blockquote>";
        let full = html_to_text(html);
        assert!(full.contains("Zeile eins"));
        assert!(full.contains("Zeile zwei zitiert")); // full text keeps the quote
    }
}
