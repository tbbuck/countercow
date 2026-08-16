//! Payload encoding for the Diagnostics IPC protocol.
//!
//! Everything is little-endian. The only subtle part is `string`, which the reference client
//! (`BinaryWriterExtensions.WriteString`) encodes as an `i32` count of UTF-16 code units
//! *including* the NUL terminator, followed by that many UTF-16LE units, terminator included.

/// Builds a command payload. Numbers are little-endian; see module docs for string rules.
#[derive(Debug, Default)]
pub struct PayloadWriter {
    buf: Vec<u8>,
}

impl PayloadWriter {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.buf.push(u8::from(v));
        self
    }

    /// A non-null string. Note that `""` encodes to 6 bytes (length 1, one lone NUL), not 4 —
    /// the 4-byte form means *null*. The reference client always emits this 6-byte form for an
    /// absent filterData, so we match it byte-for-byte.
    pub fn string(&mut self, s: &str) -> &mut Self {
        let units: Vec<u16> = s.encode_utf16().collect();
        // Length counts UTF-16 code units, so a non-BMP char counts as 2. Not `str::len()`
        // (bytes) and not `chars().count()` (scalar values).
        self.u32(units.len() as u32 + 1);
        for unit in units {
            self.buf.extend_from_slice(&unit.to_le_bytes());
        }
        self.buf.extend_from_slice(&[0, 0]);
        self
    }

    /// An explicit null string: just a zero length, with nothing following.
    pub fn null_string(&mut self) -> &mut Self {
        self.u32(0)
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_are_little_endian() {
        let mut w = PayloadWriter::new();
        w.u32(256).u64(1).bool(true).bool(false);
        assert_eq!(
            w.into_bytes(),
            vec![0x00, 0x01, 0x00, 0x00, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0]
        );
    }

    #[test]
    fn ascii_string_counts_the_terminator() {
        let mut w = PayloadWriter::new();
        w.string("ab");
        // length 3 = 2 chars + NUL, then 3 UTF-16LE units
        assert_eq!(
            w.into_bytes(),
            vec![3, 0, 0, 0, b'a', 0, b'b', 0, 0, 0]
        );
    }

    #[test]
    fn empty_string_is_six_bytes_but_null_is_four() {
        let mut empty = PayloadWriter::new();
        empty.string("");
        assert_eq!(empty.into_bytes(), vec![1, 0, 0, 0, 0, 0]);

        let mut null = PayloadWriter::new();
        null.null_string();
        assert_eq!(null.into_bytes(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn non_bmp_chars_count_as_two_units() {
        let mut w = PayloadWriter::new();
        // U+1F600 is a surrogate pair: 2 UTF-16 units, so length is 3 including the NUL,
        // even though the string is 1 char and 4 UTF-8 bytes.
        w.string("\u{1F600}");
        let bytes = w.into_bytes();
        assert_eq!(&bytes[0..4], &[3, 0, 0, 0]);
        assert_eq!(bytes.len(), 4 + 3 * 2);
        assert_eq!(&bytes[4..8], &[0x3D, 0xD8, 0x00, 0xDE]);
    }

    #[test]
    fn collect_tracing2_worked_example_matches_the_spec() {
        // 256MB circular buffer, NetTrace format, no rundown, one provider:
        // System.Runtime at Informational with EventCounterIntervalSec=1.
        // The reference byte dump for this is 115 payload bytes.
        let mut w = PayloadWriter::new();
        w.u32(256)
            .u32(1)
            .bool(false)
            .u32(1)
            .u64(0)
            .u32(4)
            .string("System.Runtime")
            .string("EventCounterIntervalSec=1");

        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), 115, "payload should be 115 bytes");
        // Fixed fields occupy 4+4+1+4 = 13 bytes, then keywords (8) and level (4).
        // name length at 25: 14 chars + NUL = 15
        assert_eq!(&bytes[25..29], &[0x0F, 0, 0, 0]);
        assert_eq!(&bytes[29..33], b"S\0y\0");
        // filterData length at 59 (25 + 4 + 30): 25 chars + NUL = 26 = 0x1A
        assert_eq!(&bytes[59..63], &[0x1A, 0, 0, 0]);
        assert_eq!(&bytes[63..67], b"E\0v\0");
    }
}
