//! Splits a byte stream into complete lines. Network chunks can divide a line anywhere, including
//! in the middle of a multi-byte character, so decoding has to wait for the whole line.

#[derive(Debug, Default)]
pub(crate) struct LineDecoder {
    pending: Vec<u8>,
}

impl LineDecoder {
    /// Adds bytes and returns every line they complete, without line endings or blank lines.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(bytes);
        let Some(last_newline) = self.pending.iter().rposition(|&byte| byte == b'\n') else {
            return Vec::new();
        };
        let rest = self.pending.split_off(last_newline + 1);
        let complete = std::mem::replace(&mut self.pending, rest);
        complete
            .split(|&byte| byte == b'\n')
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
            .filter(|line| !line.is_empty())
            .map(<[u8]>::to_vec)
            .collect()
    }

    /// Returns any final line that had no line ending.
    pub(crate) fn finish(&mut self) -> Option<Vec<u8>> {
        let rest = std::mem::take(&mut self.pending);
        (!rest.is_empty()).then_some(rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_the_same_wherever_the_chunks_divide() {
        let input = "{\"a\":\"é\"}\r\n\n{\"b\":2}\nlast".as_bytes();
        for split in 0..=input.len() {
            let mut decoder = LineDecoder::default();
            let mut lines = decoder.push(&input[..split]);
            lines.extend(decoder.push(&input[split..]));
            lines.extend(decoder.finish());
            assert_eq!(lines, ["{\"a\":\"é\"}".as_bytes(), b"{\"b\":2}", b"last"], "split at {split}");
        }
    }
}
