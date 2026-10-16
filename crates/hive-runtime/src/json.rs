//! Exact JSON byte accounting without allocating an encoded copy of a context.

use std::io::{self, Write};

/// Uses the same serializer as wire/storage encoding, including UTF-8 and
/// escaping. Only the byte count is retained, even for large histories.
pub fn encoded_len<T: serde::Serialize + ?Sized>(value: &T) -> serde_json::Result<usize> {
    let mut counter = ByteCounter(0);
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}

struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("encoded JSON size overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_encoded_bytes_including_escaping_and_private_continuation() {
        use crate::providers::inference::{Continuation, InferenceMessage, MessageRole};
        let mut message = InferenceMessage::text(
            MessageRole::Assistant,
            "block".into(),
            "Unicode ☃, quotes \" and control characters: \n\t\u{1}".into(),
        );
        message.continuation = Some(Continuation {
            scope: "scope".into(),
            provider: "codex".into(),
            model: "model".into(),
            wire: "responses".into(),
            required: true,
            items: vec![serde_json::json!({"encrypted_content": "opaque\n\"☃"})],
        });
        let messages = vec![message];
        assert_eq!(
            encoded_len(&messages).unwrap(),
            serde_json::to_vec(&messages).unwrap().len()
        );
    }
}
