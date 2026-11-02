// SSE (Server-Sent Events) record splitter/parser shared by the dashboard
// chat page's streaming reader.
//
// wave3b / F07-5: the previous parser normalized CRLF -> LF independently
// on each decoded network chunk, then looked for "\n\n" in the accumulated
// buffer. When a CR/LF pair straddled two chunks (e.g. one chunk ends
// "...\r\n\r" and the next begins "\n"), per-chunk normalization never saw
// the two halves together, so the terminal blank-line separator was never
// recognised and a completed `done` event could be missed — the stream
// reader would fall through to "stream ended without an answer" or replay
// the question through the blocking fallback.
//
// Fix: normalize on the ACCUMULATED buffer (not per chunk), and hold back a
// single trailing bare `\r` across a push() call — it might be the first
// half of a CRLF whose `\n` hasn't arrived yet — so it only gets resolved
// (collapsed into the preceding line, or turned into its own line ending)
// once more input confirms what it is.

export function createSseRecordSplitter() {
    let buffer = '';

    // Feed one more decoded text chunk in. Returns an array of zero or more
    // complete raw records — the text between two blank-line separators,
    // newline-normalized, with the separator itself stripped.
    function push(chunk) {
        buffer += chunk;

        // A lone trailing \r is ambiguous: it is either a bare-CR line
        // ending, or the first half of a CRLF pair whose \n hasn't arrived
        // yet. Hold it out of this pass; the next push() (or EOF) resolves
        // it either by seeing the \n (CRLF -> single \n) or by seeing
        // anything else (the \r stands as its own line ending).
        let pending = '';
        if (buffer.endsWith('\r')) {
            pending = '\r';
            buffer = buffer.slice(0, -1);
        }

        // Normalize every remaining line ending in the accumulated buffer
        // to \n before splitting on the \n\n record separator.
        buffer = buffer.replace(/\r\n/g, '\n').replace(/\r/g, '\n');

        const records = [];
        let sep;
        while ((sep = buffer.indexOf('\n\n')) !== -1) {
            records.push(buffer.slice(0, sep));
            buffer = buffer.slice(sep + 2);
        }

        buffer += pending;
        return records;
    }

    // Whatever is left in the buffer once the stream has ended (normally
    // nothing — a well-formed SSE stream's last record is still followed by
    // its own blank-line separator before the connection closes).
    function remainder() {
        return buffer;
    }

    return { push, remainder };
}

// Parse one raw SSE record (the text between two \n\n separators, already
// newline-normalized, no trailing newline) into { event, dataLines }.
// `event` defaults to 'message' per the SSE spec.
export function parseSseRecord(rawRecord) {
    let event = 'message';
    const dataLines = [];
    for (const line of rawRecord.split('\n')) {
        if (line.startsWith('event:')) event = line.slice(6).trim();
        else if (line.startsWith('data:')) dataLines.push(line.slice(5).replace(/^ /, ''));
    }
    return { event, dataLines };
}
