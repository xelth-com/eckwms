// Minimal, safe markdown renderer for AI chat answers.
//
// Contract: HTML is escaped FIRST, unconditionally — the input is model
// output and must never reach the DOM as live markup. Formatting is then
// layered on top of the already-escaped text, so even a model that emits
// literal HTML (e.g. "<script>alert(1)</script>") only ever produces
// visible, inert text.
//
// Supported: paragraphs, line breaks, **bold**, `inline code`, fenced code
// blocks (```), bullet and numbered lists, simple pipe tables, ATX headings
// (# .. ######) and [text](url) links — http(s)/mailto only; anything else
// is left as plain text rather than turned into a clickable link.
//
// Security notes (wave3b / F07-1, F08-11 — the renderer previously allowed
// event-handler attribute injection):
//  - Fenced code blocks are recognised ONLY as block syntax: the opening
//    and closing ``` must each be the ENTIRE (trimmed) line. A ``` that
//    merely appears mid-line — e.g. inside a link destination — is never
//    treated as a fence, so code content can never originate from inside
//    an inline construct like a link.
//  - Code blocks are extracted and rendered to final HTML directly inside
//    the single top-to-bottom line scan, BEFORE any inline formatting
//    (links, bold, inline code) runs on later lines. There is no
//    placeholder/token + second-pass splice step, so there is nothing for
//    a later `String.replace(token, html)` to misinterpret as a
//    replacement-string pattern (`$&`, `` $` ``, `$'`) — that whole
//    mechanism (the root cause of F07-1/F08-11) has been removed.
//  - Link destinations reject the placeholder/quote/backtick/whitespace
//    characters that would let a destination value escape the `href="…"`
//    attribute it is rendered into, as defense in depth even though no
//    placeholder exists anymore.

function escapeHtml(text) {
    return String(text ?? '')
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;');
}

// A link destination may not contain whitespace (incl. newlines — `\s`),
// the closing paren, a quote, a backtick, or NUL — anything that could let
// the value break out of the double-quoted `href="…"` attribute it is
// rendered into.
const LINK_DEST = '[^\\s)"\'`\\u0000]+';
const LINK_RE = new RegExp(`\\[([^\\]\\n]+)\\]\\(((?:https?:\\/\\/|mailto:)${LINK_DEST})\\)`, 'g');

// Inline formatting applied within a single block (paragraph text, list
// item, table cell, heading). Operates on already-escaped text only, and
// never on fenced code-block content (that path never calls this).
function renderInline(escaped) {
    return escaped
        .replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>')
        .replace(/`([^`\n]+)`/g, '<code>$1</code>')
        .replace(LINK_RE, '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>');
}

function renderParagraph(lines) {
    return `<p>${renderInline(lines.join('\n')).replace(/\n/g, '<br>')}</p>`;
}

function renderList(items, ordered) {
    const tag = ordered ? 'ol' : 'ul';
    const lis = items.map((it) => `<li>${renderInline(it)}</li>`).join('');
    return `<${tag}>${lis}</${tag}>`;
}

function isTableSeparator(line) {
    return /^\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)*\|?$/.test(line.trim());
}

function splitTableRow(line) {
    let row = line.trim();
    if (row.startsWith('|')) row = row.slice(1);
    if (row.endsWith('|')) row = row.slice(0, -1);
    return row.split('|').map((c) => c.trim());
}

function renderTable(headerLine, bodyLines) {
    const headers = splitTableRow(headerLine);
    const headHtml = `<tr>${headers.map((h) => `<th>${renderInline(h)}</th>`).join('')}</tr>`;
    const bodyHtml = bodyLines
        .map((line) => {
            const cells = splitTableRow(line);
            return `<tr>${cells.map((c) => `<td>${renderInline(c)}</td>`).join('')}</tr>`;
        })
        .join('');
    return `<table><thead>${headHtml}</thead><tbody>${bodyHtml}</tbody></table>`;
}

// A fence delimiter is recognised ONLY when it is the entire trimmed line:
// an opening fence is ``` optionally followed by a language tag (no
// backticks allowed in it); a closing fence is a bare ```. Anything with
// a ``` elsewhere on the line (inside a link, a sentence, …) is not a fence
// at all — it falls through to ordinary paragraph/inline handling.
const FENCE_OPEN_RE = /^```([^`\n]*)$/;
const FENCE_CLOSE = '```';

export function renderChatText(text) {
    const escaped = escapeHtml(text);
    const lines = escaped.split('\n');
    const html = [];
    let para = [];
    let list = null; // { ordered, items }

    function flushParagraph() {
        if (para.length) {
            html.push(renderParagraph(para));
            para = [];
        }
    }
    function flushList() {
        if (list) {
            html.push(renderList(list.items, list.ordered));
            list = null;
        }
    }

    for (let i = 0; i < lines.length; i++) {
        const line = lines[i];
        const trimmed = line.trim();

        if (trimmed === '') {
            flushParagraph();
            flushList();
            continue;
        }

        const fenceOpen = FENCE_OPEN_RE.exec(trimmed);
        if (fenceOpen) {
            flushParagraph();
            flushList();
            const codeLines = [];
            let j = i + 1;
            while (j < lines.length && lines[j].trim() !== FENCE_CLOSE) {
                codeLines.push(lines[j]);
                j++;
            }
            // Code content is already-escaped text with no further
            // formatting applied — rendered straight to final HTML, no
            // placeholder/splice step involved.
            html.push(`<pre><code>${codeLines.join('\n')}</code></pre>`);
            i = j; // skip past the closing fence line (or EOF if unterminated)
            continue;
        }

        const heading = /^(#{1,6})\s+(.*)$/.exec(trimmed);
        if (heading) {
            flushParagraph();
            flushList();
            const level = heading[1].length;
            html.push(`<h${level}>${renderInline(heading[2])}</h${level}>`);
            continue;
        }

        // Pipe table: a header row immediately followed by a separator row.
        if (trimmed.startsWith('|') && i + 1 < lines.length && isTableSeparator(lines[i + 1])) {
            flushParagraph();
            flushList();
            const bodyLines = [];
            let j = i + 2;
            while (j < lines.length && lines[j].trim().startsWith('|')) {
                bodyLines.push(lines[j]);
                j++;
            }
            html.push(renderTable(trimmed, bodyLines));
            i = j - 1;
            continue;
        }

        const bullet = /^[-*]\s+(.*)$/.exec(trimmed);
        if (bullet) {
            flushParagraph();
            if (!list || list.ordered) {
                flushList();
                list = { ordered: false, items: [] };
            }
            list.items.push(bullet[1]);
            continue;
        }

        const numbered = /^\d+[.)]\s+(.*)$/.exec(trimmed);
        if (numbered) {
            flushParagraph();
            if (!list || !list.ordered) {
                flushList();
                list = { ordered: true, items: [] };
            }
            list.items.push(numbered[1]);
            continue;
        }

        flushList();
        para.push(line);
    }
    flushParagraph();
    flushList();

    return html.join('');
}
