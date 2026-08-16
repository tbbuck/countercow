//! Event blob headers.
//!
//! Within a block, headers are delta-encoded against the previous row: a flag bit that is clear
//! means "reuse the previous value", not "zero". State resets at every block boundary.

use super::reader::{Reader, Result};

mod flags {
    pub const METADATA_ID: u8 = 1 << 0;
    /// Sets three fields at once: sequence number, capture thread id, processor number.
    pub const CAPTURE_THREAD_AND_SEQUENCE: u8 = 1 << 1;
    pub const THREAD_ID: u8 = 1 << 2;
    pub const STACK_ID: u8 = 1 << 3;
    pub const ACTIVITY_ID: u8 = 1 << 4;
    pub const RELATED_ACTIVITY_ID: u8 = 1 << 5;
    /// Not a presence bit — this *is* the IsSorted value.
    pub const IS_SORTED: u8 = 1 << 6;
    pub const PAYLOAD_SIZE: u8 = 1 << 7;
}

#[derive(Debug, Clone, Default)]
pub struct EventHeader {
    pub metadata_id: u32,
    pub sequence_number: u32,
    pub capture_thread_id: u64,
    pub processor_number: u32,
    pub thread_id: u64,
    pub stack_id: u32,
    pub timestamp: u64,
    pub activity_id: [u8; 16],
    pub related_activity_id: [u8; 16],
    pub is_sorted: bool,
    pub payload_size: u32,
}

impl EventHeader {
    /// Read one compressed header, updating `self` in place so it carries forward as the
    /// previous-row state for the next call.
    pub fn read_compressed(&mut self, r: &mut Reader<'_>) -> Result<()> {
        let flags = r.u8()?;

        if flags & flags::METADATA_ID != 0 {
            self.metadata_id = r.varuint32()?;
        }

        if flags & flags::CAPTURE_THREAD_AND_SEQUENCE != 0 {
            // The delta is stored one below the true increment.
            self.sequence_number = self.sequence_number.wrapping_add(r.varuint32()?).wrapping_add(1);
            self.capture_thread_id = r.varuint64()?;
            self.processor_number = r.varuint32()?;
        } else if self.metadata_id != 0 {
            self.sequence_number = self.sequence_number.wrapping_add(1);
        }

        if flags & flags::THREAD_ID != 0 {
            self.thread_id = r.varuint64()?;
        }
        if flags & flags::STACK_ID != 0 {
            self.stack_id = r.varuint32()?;
        }

        // Always present, always a delta against the previous row.
        self.timestamp = self.timestamp.wrapping_add(r.varuint64()?);

        if flags & flags::ACTIVITY_ID != 0 {
            self.activity_id.copy_from_slice(r.bytes(16, "activity id")?);
        }
        if flags & flags::RELATED_ACTIVITY_ID != 0 {
            self.related_activity_id
                .copy_from_slice(r.bytes(16, "related activity id")?);
        }

        self.is_sorted = flags & flags::IS_SORTED != 0;

        if flags & flags::PAYLOAD_SIZE != 0 {
            self.payload_size = r.varuint32()?;
        }

        Ok(())
    }

    /// Read one uncompressed header.
    ///
    /// Unreachable in practice: the runtime sets the compression flag unconditionally for
    /// nettrace V4, so nothing shipping emits this. Implemented for completeness, and it fails
    /// loudly rather than silently if the layout is ever wrong.
    pub fn read_uncompressed(&mut self, r: &mut Reader<'_>) -> Result<()> {
        let _event_size = r.u32()?;
        let raw_metadata_id = r.u32()?;
        // The published spec says the high bit *is* IsSorted; both the runtime writer and the
        // TraceEvent reader say the opposite, so a set high bit means NOT sorted.
        self.metadata_id = raw_metadata_id & 0x7FFF_FFFF;
        self.is_sorted = raw_metadata_id & 0x8000_0000 == 0;
        self.sequence_number = r.u32()?;
        self.thread_id = r.u64()?;
        self.capture_thread_id = r.u64()?;
        self.processor_number = r.u32()?;
        self.stack_id = r.u32()?;
        self.timestamp = r.u64()?;
        self.activity_id.copy_from_slice(r.bytes(16, "activity id")?);
        self.related_activity_id
            .copy_from_slice(r.bytes(16, "related activity id")?);
        self.payload_size = r.u32()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut v: u64) -> Vec<u8> {
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

    #[test]
    fn minimal_row_carries_everything_forward() {
        // Only the timestamp delta is present.
        let mut bytes = vec![0u8];
        bytes.extend(varint(500));

        let mut header = EventHeader {
            metadata_id: 3,
            thread_id: 99,
            payload_size: 42,
            timestamp: 1_000,
            sequence_number: 5,
            ..Default::default()
        };
        header.read_compressed(&mut Reader::new(&bytes)).unwrap();

        assert_eq!(header.timestamp, 1_500, "timestamp is a delta");
        assert_eq!(header.metadata_id, 3, "retained");
        assert_eq!(header.thread_id, 99, "retained");
        assert_eq!(header.payload_size, 42, "retained");
        // metadata_id is non-zero, so the sequence still advances by one.
        assert_eq!(header.sequence_number, 6);
    }

    #[test]
    fn capture_thread_flag_sets_three_fields_and_offsets_the_sequence() {
        let mut bytes = vec![flags::CAPTURE_THREAD_AND_SEQUENCE];
        bytes.extend(varint(2)); // sequence delta; true increment is 3
        bytes.extend(varint(77)); // capture thread id
        bytes.extend(varint(4)); // processor number
        bytes.extend(varint(10)); // timestamp delta

        let mut header = EventHeader { sequence_number: 100, ..Default::default() };
        header.read_compressed(&mut Reader::new(&bytes)).unwrap();

        assert_eq!(header.sequence_number, 103, "delta + 1");
        assert_eq!(header.capture_thread_id, 77);
        assert_eq!(header.processor_number, 4);
        assert_eq!(header.timestamp, 10);
    }

    #[test]
    fn sequence_does_not_advance_when_metadata_id_is_zero() {
        // Metadata rows carry id 0 and must not bump the sequence.
        let mut bytes = vec![0u8];
        bytes.extend(varint(1));

        let mut header = EventHeader { metadata_id: 0, sequence_number: 9, ..Default::default() };
        header.read_compressed(&mut Reader::new(&bytes)).unwrap();
        assert_eq!(header.sequence_number, 9);
    }

    #[test]
    fn all_fields_present_decodes_in_order() {
        let mut bytes = vec![
            flags::METADATA_ID
                | flags::CAPTURE_THREAD_AND_SEQUENCE
                | flags::THREAD_ID
                | flags::STACK_ID
                | flags::ACTIVITY_ID
                | flags::RELATED_ACTIVITY_ID
                | flags::IS_SORTED
                | flags::PAYLOAD_SIZE,
        ];
        bytes.extend(varint(11)); // metadata id
        bytes.extend(varint(0)); // sequence delta
        bytes.extend(varint(22)); // capture thread
        bytes.extend(varint(1)); // processor
        bytes.extend(varint(33)); // thread id
        bytes.extend(varint(44)); // stack id
        bytes.extend(varint(55)); // timestamp delta
        bytes.extend([0xAA; 16]); // activity id
        bytes.extend([0xBB; 16]); // related activity id
        bytes.extend(varint(66)); // payload size

        let mut header = EventHeader::default();
        header.read_compressed(&mut Reader::new(&bytes)).unwrap();

        assert_eq!(header.metadata_id, 11);
        assert_eq!(header.sequence_number, 1);
        assert_eq!(header.capture_thread_id, 22);
        assert_eq!(header.processor_number, 1);
        assert_eq!(header.thread_id, 33);
        assert_eq!(header.stack_id, 44);
        assert_eq!(header.timestamp, 55);
        assert_eq!(header.activity_id, [0xAA; 16]);
        assert_eq!(header.related_activity_id, [0xBB; 16]);
        assert!(header.is_sorted);
        assert_eq!(header.payload_size, 66);
    }

    #[test]
    fn is_sorted_consumes_no_bytes() {
        // Same row twice, once with the IsSorted bit: both must read the same field values.
        let mut plain = vec![0u8];
        plain.extend(varint(7));
        let mut sorted = vec![flags::IS_SORTED];
        sorted.extend(varint(7));

        let mut a = EventHeader::default();
        a.read_compressed(&mut Reader::new(&plain)).unwrap();
        let mut b = EventHeader::default();
        b.read_compressed(&mut Reader::new(&sorted)).unwrap();

        assert_eq!(a.timestamp, b.timestamp);
        assert!(!a.is_sorted);
        assert!(b.is_sorted);
    }

    #[test]
    fn consecutive_rows_accumulate_timestamps() {
        let mut bytes = Vec::new();
        for delta in [100u64, 50, 25] {
            bytes.push(0);
            bytes.extend(varint(delta));
        }

        let mut r = Reader::new(&bytes);
        let mut header = EventHeader::default();
        let mut seen = Vec::new();
        for _ in 0..3 {
            header.read_compressed(&mut r).unwrap();
            seen.push(header.timestamp);
        }
        assert_eq!(seen, vec![100, 150, 175]);
    }
}
