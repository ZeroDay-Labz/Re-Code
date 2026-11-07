//! Reasoning that arrives as plain text. Some servers (Gemini's OpenAI-compatible endpoint, gateways
//! in front of open models) don't separate a model's thinking from its answer: it comes inside the
//! content as `<thought>…</thought>` or `<think>…</think>`. This filter routes what's inside such tags
//! to the reasoning stream and keeps the rest as text, holding back a partial tag at the end of a
//! chunk until the next one shows what it was.

use crate::StreamEvent;

const OPEN: &[&str] = &["<think>", "<thought>", "<thinking>"];
const CLOSE: &[&str] = &["</think>", "</thought>", "</thinking>"];

#[derive(Debug, Default)]
pub struct TagFilter {
    inside: bool,
    pending: String,
}

impl TagFilter {
    /// Takes a text delta and returns what can be emitted now.
    pub fn push(&mut self, text: &str) -> Vec<StreamEvent> {
        self.pending.push_str(text);
        let mut out = Vec::new();
        loop {
            let tags = if self.inside { CLOSE } else { OPEN };
            let found = tags
                .iter()
                .filter_map(|tag| self.pending.find(tag).map(|at| (at, tag.len())))
                .min_by_key(|(at, _)| *at);
            match found {
                Some((at, len)) => {
                    let before = self.pending[..at].to_owned();
                    self.emit(&mut out, before);
                    self.pending.drain(..at + len);
                    self.inside = !self.inside;
                }
                None => {
                    // Keep a tail that could still become one of the tags; emit everything before it.
                    let keep =
                        tags.iter().filter_map(|tag| longest_prefix_at_end(&self.pending, tag)).max().unwrap_or(0);
                    let cut = self.pending.len() - keep;
                    let ready = self.pending[..cut].to_owned();
                    self.pending.drain(..cut);
                    self.emit(&mut out, ready);
                    return out;
                }
            }
        }
    }

    /// Whatever is still held back, once the stream has ended.
    pub fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.pending);
        self.emit(&mut out, rest);
        out
    }

    fn emit(&self, out: &mut Vec<StreamEvent>, text: String) {
        if text.is_empty() {
            return;
        }
        out.push(if self.inside { StreamEvent::ReasoningDelta(text) } else { StreamEvent::TextDelta(text) });
    }
}

/// How many bytes at the end of `text` are a proper prefix of `tag` (0 when none).
fn longest_prefix_at_end(text: &str, tag: &str) -> Option<usize> {
    (1..tag.len()).rev().find(|&len| text.ends_with(&tag[..len]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> Vec<StreamEvent> {
        let mut filter = TagFilter::default();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(filter.push(chunk));
        }
        out.extend(filter.finish());
        out
    }

    fn joined(events: &[StreamEvent]) -> (String, String) {
        let mut text = String::new();
        let mut reasoning = String::new();
        for event in events {
            match event {
                StreamEvent::TextDelta(t) => text.push_str(t),
                StreamEvent::ReasoningDelta(t) => reasoning.push_str(t),
                _ => {}
            }
        }
        (text, reasoning)
    }

    #[test]
    fn thinking_inside_tags_becomes_reasoning_even_across_chunks() {
        let events = run(&["<thou", "ght>The user asks", " a question.</thou", "ght>Hello there!"]);
        assert_eq!(joined(&events), ("Hello there!".to_owned(), "The user asks a question.".to_owned()));
        let events = run(&["plain ", "<think>deep</think>", " answer"]);
        assert_eq!(joined(&events), ("plain  answer".to_owned(), "deep".to_owned()));
    }

    #[test]
    fn text_without_tags_passes_through_untouched_including_angle_brackets() {
        let events = run(&["a < b and ", "<b>bold</b> ", "and x <thin"]);
        let (text, reasoning) = joined(&events);
        assert_eq!(text, "a < b and <b>bold</b> and x <thin", "an unfinished prefix is released at the end");
        assert_eq!(reasoning, "");
    }

    #[test]
    fn an_unclosed_tag_keeps_the_rest_as_reasoning() {
        let events = run(&["<think>never closed", " at all"]);
        assert_eq!(joined(&events), (String::new(), "never closed at all".to_owned()));
    }
}
