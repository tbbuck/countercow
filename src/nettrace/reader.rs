//! Byte-level reading primitives for the nettrace stream.
//!
//! Two readers, because the format needs both:
//!
//! * [`StreamReader`] sits on the socket and does the outer framing. It tracks the absolute
//!   stream offset, which block padding is computed from.
//! * [`Reader`] parses a block's contents once they are in memory.

use std::fmt;
use std::io::Read;

#[derive(Debug)]
pub enum ParseError {
    Io(std::io::Error),
    /// Ran out of bytes mid-structure.
    Truncated { what: &'static str, wanted: usize, remaining: usize },
    /// A varint ran past its maximum encoded length.
    MalformedVarint,
    /// A structural byte was not one of the values the format allows.
    Unexpected { what: &'static str, got: String },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Io(e) => write!(f, "nettrace stream I/O failed: {e}"),
            ParseError::Truncated { what, wanted, remaining } => write!(
                f,
                "truncated nettrace stream reading {what}: wanted {wanted} bytes, {remaining} remain"
            ),
            ParseError::MalformedVarint => write!(f, "malformed varint in nettrace stream"),
            ParseError::Unexpected { what, got } => {
                write!(f, "unexpected {what} in nettrace stream: {got}")
            }
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ParseError {
    fn from(e: std::io::Error) -> Self {
        ParseError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, ParseError>;

/// Reads the outer stream, tracking absolute position for alignment arithmetic.
pub struct StreamReader<R: Read> {
    inner: R,
    pos: u64,
}

impl<R: Read> StreamReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner, pos: 0 }
    }

    /// Absolute byte offset from the start of the stream. Block padding is computed from this.
    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn read_exact_bytes(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.inner.read_exact(&mut buf)?;
        self.pos += n as u64;
        Ok(buf)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        // Chunked so a bogus length cannot force a huge allocation.
        let mut left = n;
        let mut scratch = [0u8; 4096];
        while left > 0 {
            let take = left.min(scratch.len());
            self.inner.read_exact(&mut scratch[..take])?;
            self.pos += take as u64;
            left -= take;
        }
        Ok(())
    }

    pub fn u8(&mut self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.inner.read_exact(&mut b)?;
        self.pos += 1;
        Ok(b[0])
    }

    pub fn i32(&mut self) -> Result<i32> {
        let b = self.read_exact_bytes(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Parses an in-memory buffer: a block's contents, or one event's payload.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn bytes(&mut self, n: usize, what: &'static str) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(ParseError::Truncated { what, wanted: n, remaining: self.remaining() });
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub fn skip(&mut self, n: usize, what: &'static str) -> Result<()> {
        self.bytes(n, what).map(|_| ())
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1, "u8")?[0])
    }

    pub fn i16(&mut self) -> Result<i16> {
        let b = self.bytes(2, "i16")?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2, "u16")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn i32(&mut self) -> Result<i32> {
        let b = self.bytes(4, "i32")?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(self.i32()? as u32)
    }

    pub fn i64(&mut self) -> Result<i64> {
        let b = self.bytes(8, "i64")?;
        Ok(i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    pub fn u64(&mut self) -> Result<u64> {
        Ok(self.i64()? as u64)
    }

    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    /// Unsigned LEB128. Despite the spec calling these "varint32/varint64" there is no sign
    /// encoding and no zigzag in nettrace V4/V5.
    pub fn varuint(&mut self, max_shift: u32) -> Result<u64> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            if shift >= max_shift {
                return Err(ParseError::MalformedVarint);
            }
            let byte = self.u8()?;
            value |= u64::from(byte & 0x7F) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
    }

    pub fn varuint32(&mut self) -> Result<u32> {
        Ok(self.varuint(35)? as u32)
    }

    pub fn varuint64(&mut self) -> Result<u64> {
        self.varuint(70)
    }

    /// UTF-16LE terminated by a 0x0000 code unit, with no length prefix. This is how strings
    /// appear inside metadata records and event payloads.
    pub fn utf16_nul_string(&mut self) -> Result<String> {
        let mut units = Vec::new();
        loop {
            let unit = self.u16()?;
            if unit == 0 {
                return Ok(String::from_utf16_lossy(&units));
            }
            units.push(unit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_scalars() {
        let bytes = [0x01, 0x02, 0x03, 0x04, 0xFF, 0xFF];
        let mut r = Reader::new(&bytes);
        assert_eq!(r.i32().unwrap(), 0x04030201);
        assert_eq!(r.i16().unwrap(), -1);
        assert!(r.is_empty());
    }

    #[test]
    fn varint_single_byte() {
        let bytes = [0x7F];
        assert_eq!(Reader::new(&bytes).varuint32().unwrap(), 127);
    }

    #[test]
    fn varint_multi_byte_boundaries() {
        // 128 => 0x80 0x01
        assert_eq!(Reader::new(&[0x80, 0x01]).varuint32().unwrap(), 128);
        // 300 => 0xAC 0x02
        assert_eq!(Reader::new(&[0xAC, 0x02]).varuint32().unwrap(), 300);
        // u32::MAX => 5 bytes
        assert_eq!(
            Reader::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F]).varuint32().unwrap(),
            u32::MAX
        );
        // u64::MAX => 10 bytes
        let max = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01];
        assert_eq!(Reader::new(&max).varuint64().unwrap(), u64::MAX);
    }

    #[test]
    fn varint_round_trips() {
        fn encode(mut v: u64) -> Vec<u8> {
            let mut out = Vec::new();
            loop {
                let byte = (v & 0x7F) as u8;
                v >>= 7;
                if v == 0 {
                    out.push(byte);
                    return out;
                }
                out.push(byte | 0x80);
            }
        }
        for value in [0u64, 1, 127, 128, 16_383, 16_384, 1 << 31, 1 << 62, u64::MAX] {
            let bytes = encode(value);
            assert_eq!(Reader::new(&bytes).varuint64().unwrap(), value, "{value}");
        }
    }

    #[test]
    fn overlong_varint_is_rejected() {
        // Eleven continuation bytes exceeds the u64 limit.
        let bytes = [0x80u8; 11];
        assert!(matches!(
            Reader::new(&bytes).varuint64(),
            Err(ParseError::MalformedVarint)
        ));
    }

    #[test]
    fn reads_nul_terminated_utf16() {
        let mut bytes = Vec::new();
        for unit in "System.Runtime".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&[0xAB, 0xCD]); // trailing data must be left alone

        let mut r = Reader::new(&bytes);
        assert_eq!(r.utf16_nul_string().unwrap(), "System.Runtime");
        assert_eq!(r.remaining(), 2);
    }

    #[test]
    fn empty_utf16_string_is_just_the_terminator() {
        let bytes = [0u8, 0];
        assert_eq!(Reader::new(&bytes).utf16_nul_string().unwrap(), "");
    }

    #[test]
    fn truncation_reports_context() {
        let bytes = [1u8, 2];
        match Reader::new(&bytes).i32() {
            Err(ParseError::Truncated { what, wanted, remaining }) => {
                assert_eq!((what, wanted, remaining), ("i32", 4, 2));
            }
            other => panic!("expected truncation, got {other:?}"),
        }
    }

    #[test]
    fn stream_reader_tracks_absolute_position() {
        let data = vec![0u8; 100];
        let mut r = StreamReader::new(std::io::Cursor::new(data));
        r.read_exact_bytes(8).unwrap();
        assert_eq!(r.position(), 8);
        r.skip(5).unwrap();
        assert_eq!(r.position(), 13);
        r.u8().unwrap();
        assert_eq!(r.position(), 14);
    }
}
