//! An incremental Server-Sent Events parser (the `text/event-stream` format).
//!
//! Bytes arrive in arbitrary chunks; [`Parser::feed`] buffers partial lines
//! (and partial UTF-8) and returns every event completed by the chunk.

/// One dispatched event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field; empty means the default `message`.
    pub event: String,
    /// The `data:` lines joined with `\n`.
    pub data: String,
}

#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    event: String,
    data: String,
    has_data: bool,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk; returns the events it completed.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(rel) = self.buf[start..].iter().position(|&b| b == b'\n' || b == b'\r') {
            let end = start + rel;
            // A lone `\r` at the end of the buffer may be the first half of `\r\n`.
            if self.buf[end] == b'\r' && end + 1 == self.buf.len() {
                break;
            }
            let line = String::from_utf8_lossy(&self.buf[start..end]).into_owned();
            let mut next = end + 1;
            if self.buf[end] == b'\r' && self.buf.get(next) == Some(&b'\n') {
                next += 1;
            }
            start = next;
            if let Some(ev) = self.line(&line) {
                out.push(ev);
            }
        }
        self.buf.drain(..start);
        out
    }

    /// End of stream: dispatch a final event that had no blank line after it.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
            let line = line.trim_end_matches(['\r', '\n']).to_owned();
            if let Some(ev) = self.line(&line) {
                return Some(ev);
            }
        }
        self.dispatch()
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None; // comment / keep-alive
        }
        let (field, value) = match line.find(':') {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            _ => {} // id, retry: not needed
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = std::mem::take(&mut self.event);
        if !self.has_data {
            return None;
        }
        self.has_data = false;
        Some(SseEvent { event, data: std::mem::take(&mut self.data) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_split_across_chunks() {
        let text = "event: ping\ndata: {\"a\":1}\n\n: keep-alive\n\nevent: delta\r\ndata: one\r\ndata: two\r\n\r\n";
        // Byte-by-byte, so every split point is exercised.
        let mut p = Parser::new();
        let mut got = Vec::new();
        for b in text.as_bytes() {
            got.extend(p.feed(std::slice::from_ref(b)));
        }
        assert_eq!(
            got,
            vec![
                SseEvent { event: "ping".into(), data: "{\"a\":1}".into() },
                SseEvent { event: "delta".into(), data: "one\ntwo".into() },
            ]
        );
        assert_eq!(p.finish(), None);
    }

    #[test]
    fn keeps_utf8_split_inside_a_character() {
        let text = "data: café ♪\n\n".as_bytes();
        let mut p = Parser::new();
        let mid = text.len() - 4; // inside the 3-byte ♪
        let mut got = p.feed(&text[..mid]);
        got.extend(p.feed(&text[mid..]));
        assert_eq!(got[0].data, "café ♪");
    }

    #[test]
    fn finish_flushes_an_unterminated_event() {
        let mut p = Parser::new();
        assert!(p.feed(b"event: x\ndata: tail").is_empty());
        assert_eq!(p.finish(), Some(SseEvent { event: "x".into(), data: "tail".into() }));
    }
}
