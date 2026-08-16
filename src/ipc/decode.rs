//! Reading command *response* payloads. Same wire format as [`super::encode`], in reverse.

use std::fmt;

#[derive(Debug)]
pub struct TruncatedPayload {
    pub wanted: usize,
    pub remaining: usize,
}

impl fmt::Display for TruncatedPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "truncated response payload: wanted {} more bytes, {} remain",
            self.wanted, self.remaining
        )
    }
}

impl std::error::Error for TruncatedPayload {}

type Result<T> = std::result::Result<T, TruncatedPayload>;

pub struct PayloadReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> PayloadReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(TruncatedPayload { wanted: n, remaining: self.remaining() });
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    /// A .NET GUID: 16 raw bytes, not RFC-4122 byte order. We only need to skip it.
    pub fn guid(&mut self) -> Result<[u8; 16]> {
        let b = self.take(16)?;
        let mut out = [0u8; 16];
        out.copy_from_slice(b);
        Ok(out)
    }

    /// Length-prefixed UTF-16LE, where the length counts code units including the terminator.
    /// A length of 0 means null, which we surface as an empty string.
    pub fn string(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        if len == 0 {
            return Ok(String::new());
        }
        let bytes = self.take(len * 2)?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            // Drop the trailing NUL.
            .take(len - 1)
            .collect();
        Ok(String::from_utf16_lossy(&units))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::encode::PayloadWriter;

    #[test]
    fn round_trips_scalars_and_strings() {
        let mut w = PayloadWriter::new();
        w.u32(42).u64(7).string("System.Runtime").string("");

        let bytes = w.into_bytes();
        let mut r = PayloadReader::new(&bytes);
        assert_eq!(r.u32().unwrap(), 42);
        assert_eq!(r.u64().unwrap(), 7);
        assert_eq!(r.string().unwrap(), "System.Runtime");
        assert_eq!(r.string().unwrap(), "");
        assert!(r.is_empty());
    }

    #[test]
    fn round_trips_non_bmp() {
        let mut w = PayloadWriter::new();
        w.string("hej \u{1F600}");
        let bytes = w.into_bytes();
        assert_eq!(PayloadReader::new(&bytes).string().unwrap(), "hej \u{1F600}");
    }

    #[test]
    fn truncation_is_an_error_not_a_panic() {
        let bytes = [1u8, 0, 0];
        assert!(PayloadReader::new(&bytes).u32().is_err());

        // Claims 10 code units but supplies none.
        let bytes = [10u8, 0, 0, 0];
        assert!(PayloadReader::new(&bytes).string().is_err());
    }

    #[test]
    fn null_string_reads_as_empty() {
        let bytes = [0u8, 0, 0, 0];
        assert_eq!(PayloadReader::new(&bytes).string().unwrap(), "");
    }
}
