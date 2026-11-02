import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSseRecordSplitter, parseSseRecord } from './sseParser.js';

const DONE_EVENT = 'event: done\r\ndata: {"answer":"hi"}\r\n\r\n';

test('plain LF stream: one push, one record', () => {
    const splitter = createSseRecordSplitter();
    const records = splitter.push('event: done\ndata: {"answer":"hi"}\n\n');
    assert.equal(records.length, 1);
    const { event, dataLines } = parseSseRecord(records[0]);
    assert.equal(event, 'done');
    assert.deepEqual(JSON.parse(dataLines.join('\n')), { answer: 'hi' });
});

test('CRLF stream delivered in one push: one record', () => {
    const splitter = createSseRecordSplitter();
    const records = splitter.push(DONE_EVENT);
    assert.equal(records.length, 1);
    const { event } = parseSseRecord(records[0]);
    assert.equal(event, 'done');
});

// F07-5: feed a CRLF `done` event split at every possible byte boundary
// across two chunks and require exactly one record out, with nothing lost.
test('F07-5: CRLF done event split at every byte boundary yields exactly one record', () => {
    for (let cut = 1; cut < DONE_EVENT.length; cut++) {
        const chunk1 = DONE_EVENT.slice(0, cut);
        const chunk2 = DONE_EVENT.slice(cut);
        const splitter = createSseRecordSplitter();
        const records = [...splitter.push(chunk1), ...splitter.push(chunk2)];
        assert.equal(
            records.length,
            1,
            `split at byte ${cut} (${JSON.stringify(chunk1)} | ${JSON.stringify(chunk2)}) produced ${records.length} records`
        );
        const { event, dataLines } = parseSseRecord(records[0]);
        assert.equal(event, 'done');
        assert.deepEqual(JSON.parse(dataLines.join('\n')), { answer: 'hi' });
        assert.equal(splitter.remainder(), '');
    }
});

// The specific chunking the finding reported: '...\r\n\r' then '\n'.
test('F07-5: exact reported split (...\\r\\n\\r | \\n)', () => {
    const idx = DONE_EVENT.lastIndexOf('\r\n\r\n') + 3; // split inside the final \r\n\r\n, after the first \r\n\r
    const chunk1 = DONE_EVENT.slice(0, idx);
    const chunk2 = DONE_EVENT.slice(idx);
    assert.ok(chunk1.endsWith('\r\n\r'));
    assert.equal(chunk2, '\n');
    const splitter = createSseRecordSplitter();
    const records = [...splitter.push(chunk1), ...splitter.push(chunk2)];
    assert.equal(records.length, 1);
});

test('multiple records across chunks, mixed LF/CRLF', () => {
    const splitter = createSseRecordSplitter();
    const r1 = splitter.push('event: turn\ndata: {"turn":1}\n\nevent: don');
    assert.equal(r1.length, 1);
    const r2 = splitter.push('e\r\ndata: {"answer":"ok"}\r\n\r\n');
    assert.equal(r2.length, 1);
    assert.equal(parseSseRecord(r1[0]).event, 'turn');
    assert.equal(parseSseRecord(r2[0]).event, 'done');
});

test('a bare trailing CR with no following chunk stays pending (not lost as a stray separator)', () => {
    const splitter = createSseRecordSplitter();
    const records = splitter.push('event: done\ndata: {"a":1}\n\n');
    assert.equal(records.length, 1);
    const more = splitter.push('\r'); // ambiguous bare CR, nothing after it yet
    assert.equal(more.length, 0);
});
