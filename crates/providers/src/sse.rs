//! A reader for Server-Sent Events, which both OpenAI and Anthropic use for streaming.
//!
//! Chunks from the network can split anywhere, so this buffers bytes and yields complete events.
//! An event is the `event:` name (when present) paired with the joined `data:` payload.

/// One parsed event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Turns bytes into SSE events.
#[derive(Debug, Default)]
pub struct SseDecoder {
    pending: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    /// Adds bytes and returns every event they complete. A blank line ends an event.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(index) = self.pending.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=index).take(index).collect();
            self.process(&line, &mut out);
        }
        out
    }

    /// Returns any event buffered when the stream ends without a trailing blank line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            let mut out = Vec::new();
            self.process(&line, &mut out);
            if let Some(event) = out.into_iter().next() {
                return Some(event);
            }
        }
        self.take_event()
    }

    fn process(&mut self, line: &[u8], out: &mut Vec<SseEvent>) {
        let line = String::from_utf8_lossy(line);
        let line = line.strip_suffix('\r').unwrap_or(&line);
        if line.is_empty() {
            if let Some(event) = self.take_event() {
                out.push(event);
            }
        } else if let Some(name) = line.strip_prefix("event:") {
            self.event = Some(name.trim().to_owned());
        } else if let Some(payload) = line.strip_prefix("data:") {
            self.data.push(payload.strip_prefix(' ').unwrap_or(payload).to_owned());
        }
        // Comment lines (starting ':') and other fields are ignored.
    }

    /// Emits the buffered event, if it has any data. An event with only a name and no data is dropped.
    fn take_event(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        if self.data.is_empty() {
            return None;
        }
        Some(SseEvent { event, data: std::mem::take(&mut self.data).join("\n") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(input: &str, split: usize) -> Vec<SseEvent> {
        let bytes = input.as_bytes();
        let split = split.min(bytes.len());
        let mut decoder = SseDecoder::default();
        let mut out = decoder.push(&bytes[..split]);
        out.extend(decoder.push(&bytes[split..]));
        out.extend(decoder.finish());
        out
    }

    #[test]
    fn reads_named_events_however_the_bytes_split() {
        let input = "event: message_start\ndata: {\"a\":1}\n\nevent: ping\ndata: {}\n\n";
        for split in 0..=input.len() {
            let events = all(input, split);
            assert_eq!(events.len(), 2, "split {split}");
            assert_eq!(events[0], SseEvent { event: Some("message_start".into()), data: "{\"a\":1}".into() });
            assert_eq!(events[1].event.as_deref(), Some("ping"));
        }
    }

    #[test]
    fn reads_openai_style_data_only_events_and_done() {
        for split in 0..20 {
            let events = all("data: {\"choices\":[]}\n\ndata: [DONE]\n\n", split);
            assert_eq!(events[0].data, "{\"choices\":[]}");
            assert_eq!(events[1].data, "[DONE]");
        }
    }

    #[test]
    fn multiline_data_is_joined() {
        assert_eq!(all("data: a\ndata: b\n\n", 0)[0].data, "a\nb");
    }

    #[test]
    fn a_final_event_without_a_blank_line_is_still_returned() {
        assert_eq!(all("data: last", 0)[0].data, "last");
        assert_eq!(all("data: last", 5)[0].data, "last");
    }

    #[test]
    fn carriage_returns_and_comments_are_handled() {
        let events = all(": comment\r\ndata: x\r\n\r\n", 0);
        assert_eq!(events, [SseEvent { event: None, data: "x".into() }]);
    }
}
