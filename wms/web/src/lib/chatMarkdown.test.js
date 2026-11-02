import { test } from 'node:test';
import assert from 'node:assert/strict';
import { renderChatText } from './chatMarkdown.js';

// ── D1 (wave3b, F07-1 / F08-11 blocker): adversarial payloads and the
// generic attribute guard. These run FIRST and are the ones that must fail
// against the pre-fix renderer (out.replace(token, block) interpreting
// `$&`/`` $` ``/`$'` as replacement-string patterns, and a fence accepted
// inside a link destination).

// Astra's exact F07-1 repro, with the finding's "displayed \n sequences"
// turned into real newlines.
const F07_1_PAYLOAD =
    '[safe](https://e/onmouseover=globalThis.__chatXss=1//) [x](https://e/```\n$`\n```)';

test('F07-1: fence-in-link-destination + $-backreference cannot inject an event-handler attribute', () => {
    const out = renderChatText(F07_1_PAYLOAD);
    // "onmouseover" legitimately appears as inert TEXT inside the first
    // link's own href value (https://e/onmouseover=...) — that's fine. What
    // must never happen is an on*= ATTRIBUTE on a tag, i.e. the handler
    // escaping its href and becoming live markup.
    assert.ok(!/<[^>]*\son[a-z]+\s*=/i.test(out), `an on*= attribute leaked into a tag: ${out}`);
    // No duplicated/leaked replacement-pattern output from a $-backreference
    // splice either (the renderer no longer has such a splice step at all).
    assert.ok(!out.includes('<pre><code><p>'), `preceding HTML got duplicated into output: ${out}`);
});

test('F08-11 / D1: fenced block containing $&, $` and $\' round-trips as literal text, not replacement-pattern expansion', () => {
    const out = renderChatText('```\nA$&B $` C $\' D\n```');
    // escapeHtml turns & into &amp;, so the literal payload now reads A$&amp;B ...
    assert.match(out, /<pre><code>A\$&amp;B \$`? ?C \$&#39; D<\/code><\/pre>/);
    // The placeholder-leak signature from F08-11 must never appear.
    assert.ok(!out.includes('CODEBLOCK'));
});

test('D1: a fence that only opens mid-line (inside a link destination) is not treated as a code block', () => {
    const out = renderChatText('[x](https://e/```not-a-fence)');
    assert.ok(!out.includes('<pre>'));
});

// Generic attribute guard (spec: "for every test input, scan the output for
// tags and assert the only attributes anywhere are href, target, rel,
// class, and every href starts with http://, https://, or mailto:").
const ALLOWED_ATTRS = new Set(['href', 'target', 'rel', 'class']);
function assertOnlySafeAttributes(out, label) {
    const tagRe = /<[a-zA-Z][^>]*>/g;
    let tagMatch;
    while ((tagMatch = tagRe.exec(out))) {
        const tag = tagMatch[0];
        const attrRe = /([a-zA-Z_:][-a-zA-Z0-9_:.]*)\s*=\s*"([^"]*)"/g;
        let attrMatch;
        while ((attrMatch = attrRe.exec(tag))) {
            const [, name, value] = attrMatch;
            assert.ok(
                ALLOWED_ATTRS.has(name.toLowerCase()),
                `[${label}] disallowed attribute "${name}" in tag: ${tag}`
            );
            if (name.toLowerCase() === 'href') {
                assert.ok(
                    /^(https?:\/\/|mailto:)/.test(value),
                    `[${label}] href does not start with an allowed scheme: ${value}`
                );
            }
        }
        // No bare (unquoted) attribute-looking tokens either, e.g. a stray
        // `onmouseover=` that never got wrapped in quotes.
        assert.ok(!/\s[a-zA-Z-]+\s*=\s*[^"'\s>]/.test(tag), `[${label}] unquoted attribute in tag: ${tag}`);
    }
}

const GUARD_INPUTS = [
    ['bold/code/newline', '**bold** and `code` and\nnewline'],
    ['bullet list', '- one\n- two\n- three'],
    ['numbered list', '1. first\n2. second'],
    ['table', '| a | b |\n| --- | --- |\n| 1 | 2 |'],
    ['fenced code', '```js\nconst x = 1;\n**not bold**\n```'],
    ['heading', '## Summary'],
    ['http link', 'see [docs](https://example.com/x)'],
    ['mailto link', 'see [support](mailto:ops@example.com)'],
    ['non-http link', 'see [x](javascript:alert(1))'],
    ['script tag', '<script>alert(1)</script>'],
    ['ticket number', 'see #12345 for details'],
    ['F07-1 payload', F07_1_PAYLOAD],
    ['F08-11 payload', '```\nA$&B $` C $\' D\n```'],
    ['fence-in-link-destination', '[x](https://e/```not-a-fence)'],
    ['quote in destination', '[x](https://e/"onmouseover="1)'],
    ['backtick in destination', '[x](https://e/`y`)'],
    ['whitespace-split destination', '[x](https://e/ onmouseover=1)'],
    ['nested brackets/backticks adversarial', '[a](https://x/```)[b](https://y/```\n$&\n```)'],
];

test('D1: generic attribute guard — only href/target/rel/class, href always http(s)/mailto', () => {
    for (const [label, input] of GUARD_INPUTS) {
        assertOnlySafeAttributes(renderChatText(input), label);
    }
});

// ── Pre-existing regression coverage ────────────────────────────────────

test('bold and inline code regression', () => {
    const out = renderChatText('**bold** and `code` and\nnewline');
    assert.match(out, /<strong>bold<\/strong>/);
    assert.match(out, /<code>code<\/code>/);
    assert.match(out, /<br>/);
});

test('bullet list', () => {
    const out = renderChatText('- one\n- two\n- three');
    assert.match(out, /<ul><li>one<\/li><li>two<\/li><li>three<\/li><\/ul>/);
});

test('numbered list', () => {
    const out = renderChatText('1. first\n2. second');
    assert.match(out, /<ol><li>first<\/li><li>second<\/li><\/ol>/);
});

test('table', () => {
    const out = renderChatText('| a | b |\n| --- | --- |\n| 1 | 2 |');
    assert.match(out, /<table>/);
    assert.match(out, /<th>a<\/th><th>b<\/th>/);
    assert.match(out, /<td>1<\/td><td>2<\/td>/);
});

test('fenced code block preserves content verbatim', () => {
    const out = renderChatText('```js\nconst x = 1;\n**not bold**\n```');
    assert.match(out, /<pre><code>const x = 1;\n\*\*not bold\*\*<\/code><\/pre>/);
});

test('heading', () => {
    const out = renderChatText('## Summary');
    assert.match(out, /<h2>Summary<\/h2>/);
});

test('link to http(s) url', () => {
    const out = renderChatText('see [docs](https://example.com/x)');
    assert.match(out, /<a href="https:\/\/example\.com\/x" target="_blank" rel="noopener noreferrer">docs<\/a>/);
});

test('non-http link is left as plain text, not linkified', () => {
    const out = renderChatText('see [x](javascript:alert(1))');
    assert.ok(!out.includes('<a '));
    assert.match(out, /\[x\]\(javascript:alert\(1\)\)/);
});

test('script tag is escaped, never reaches the DOM as markup', () => {
    const out = renderChatText('<script>alert(1)</script>');
    assert.ok(!out.includes('<script>'));
    assert.match(out, /&lt;script&gt;alert\(1\)&lt;\/script&gt;/);
});

test('ticket number stays plain text (no resolvable route)', () => {
    const out = renderChatText('see #12345 for details');
    assert.ok(!out.includes('<a '));
    assert.match(out, /#12345/);
});
