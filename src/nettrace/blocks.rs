//! The block loop: drives the stream, maintains the metadata registry, and yields events.

use std::io::Read;

use super::event::EventHeader;
use super::fastserial::{self, tag};
use super::metadata::{self, MetadataStore};
use super::reader::{ParseError, Reader, Result, StreamReader};

/// Header of the `Trace` object: the clock reference for every event timestamp.
#[derive(Debug, Clone, Default)]
pub struct TraceInfo {
    /// Year, month, day-of-week, day, hour, minute, second, millisecond (UTC).
    pub sync_time_utc: [i16; 8],
    pub sync_time_qpc: i64,
    /// QPC ticks per second. 1e9 on Linux/macOS.
    pub qpc_frequency: i64,
    pub pointer_size: i32,
    pub process_id: i32,
    pub processor_count: i32,
    pub sampling_rate: i32,
}

impl TraceInfo {
    /// Seconds between two QPC timestamps.
    pub fn elapsed_secs(&self, from: u64, to: u64) -> f64 {
        if self.qpc_frequency == 0 {
            return 0.0;
        }
        (to as f64 - from as f64) / self.qpc_frequency as f64
    }
}

/// One event, with its payload still encoded. Metadata is resolved by the caller via
/// [`NettraceParser::metadata`], which keeps the borrow out of the returned value.
#[derive(Debug, Clone)]
pub struct RawEvent {
    pub metadata_id: u32,
    pub timestamp: u64,
    /// Index into the stack table, or 0 when the event carries no stack. Resolving it needs
    /// StackBlock parsing, which is only worthwhile for a sampling profiler.
    pub stack_id: u32,
    pub payload: Vec<u8>,
}

/// Call stacks, keyed by the id events reference.
///
/// Two generations are kept. Ids are recycled after a sequence point, so holding every stack
/// forever means a later definition silently answers for an earlier id. But discarding the
/// previous generation outright loses every sample that referenced a stack defined just before
/// the boundary — around a sixth of them, measured. Keeping one generation back, and preferring
/// the newer definition, gets both right.
#[derive(Debug, Default)]
pub struct StackTable {
    current: std::collections::HashMap<u32, Vec<u64>>,
    previous: std::collections::HashMap<u32, Vec<u64>>,
}

impl StackTable {
    /// Frames for a stack id, innermost first.
    pub fn get(&self, stack_id: u32) -> Option<&[u64]> {
        self.current
            .get(&stack_id)
            .or_else(|| self.previous.get(&stack_id))
            .map(Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.current.len() + self.previous.len()
    }

    pub fn is_empty(&self) -> bool {
        self.current.is_empty() && self.previous.is_empty()
    }

    /// Age the current generation out. Called at each sequence point.
    pub fn rotate(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }
}

/// Compression flag in an Event/Metadata block header. The runtime sets this unconditionally
/// for nettrace V4.
const FLAG_HEADER_COMPRESSION: u16 = 1;

/// Smallest legal block header: HeaderSize + Flags + MinTimestamp + MaxTimestamp.
const MIN_BLOCK_HEADER_SIZE: u16 = 20;

/// The `Trace` object's fixed body size. It is the one object with no size prefix.
const TRACE_BODY_SIZE: usize = 48;

/// Refuse absurd block sizes rather than trying to allocate them.
const MAX_BLOCK_SIZE: i32 = 256 * 1024 * 1024;

pub struct NettraceParser<R: Read> {
    stream: StreamReader<R>,
    metadata: MetadataStore,
    trace: TraceInfo,
    finished: bool,
    /// Blocks seen by type name. Diagnostic only — useful for confirming what a provider
    /// actually sends, since skipped block types are otherwise invisible.
    block_counts: std::collections::BTreeMap<String, usize>,
    /// Stack collection is off by default: only a profiler needs it, and it is not free.
    collect_stacks: bool,
    stacks: StackTable,
    sequence_point_seen: bool,
}

impl<R: Read> NettraceParser<R> {
    /// Consume the preamble and prepare to read blocks.
    pub fn new(reader: R) -> Result<Self> {
        let mut stream = StreamReader::new(reader);
        fastserial::read_preamble(&mut stream)?;
        Ok(Self {
            stream,
            metadata: MetadataStore::new(),
            trace: TraceInfo::default(),
            finished: false,
            block_counts: Default::default(),
            collect_stacks: false,
            stacks: StackTable::default(),
            sequence_point_seen: false,
        })
    }

    /// How many blocks of each type have been seen.
    pub fn block_counts(&self) -> &std::collections::BTreeMap<String, usize> {
        &self.block_counts
    }

    /// Start retaining call stacks, so `stack_id` on an event can be resolved.
    pub fn collect_stacks(&mut self) {
        self.collect_stacks = true;
    }

    pub fn stacks(&self) -> &StackTable {
        &self.stacks
    }

    /// Whether a sequence point has been passed since this was last called, clearing the flag.
    ///
    /// A sequence point means every event before it has been emitted, so buffered samples can be
    /// resolved — and must be, because the runtime may reuse stack ids afterwards.
    pub fn take_sequence_point(&mut self) -> bool {
        std::mem::take(&mut self.sequence_point_seen)
    }

    /// Age out retained stacks. Call after resolving a sequence point's worth of samples.
    pub fn rotate_stacks(&mut self) {
        self.stacks.rotate();
    }

    pub fn metadata(&self) -> &MetadataStore {
        &self.metadata
    }

    pub fn trace_info(&self) -> &TraceInfo {
        &self.trace
    }

    /// Read forward until the next batch of events, or `None` at end of stream.
    ///
    /// Metadata blocks are absorbed into the registry rather than returned; stack and sequence
    /// point blocks are skipped entirely, since counters need neither.
    pub fn next_events(&mut self) -> Result<Option<Vec<RawEvent>>> {
        loop {
            if self.finished {
                return Ok(None);
            }

            let Some(header) = fastserial::read_object_header(&mut self.stream)? else {
                self.finished = true;
                return Ok(None);
            };

            *self.block_counts.entry(header.type_name.clone()).or_default() += 1;

            match header.type_name.as_str() {
                "Trace" | "Microsoft.DotNet.Runtime.EventPipeFile" => {
                    let body = self.stream.read_exact_bytes(TRACE_BODY_SIZE)?;
                    self.trace = parse_trace(&body)?;
                    self.expect_end_object()?;
                }
                "EventBlock" => {
                    let content = self.read_block_content()?;
                    self.expect_end_object()?;
                    let events = parse_event_rows(&content)?;
                    if !events.is_empty() {
                        return Ok(Some(events));
                    }
                }
                "MetadataBlock" => {
                    let content = self.read_block_content()?;
                    self.expect_end_object()?;
                    for row in parse_event_rows(&content)? {
                        self.metadata.insert(metadata::parse(&row.payload)?);
                    }
                }
                "StackBlock" if self.collect_stacks => {
                    let content = self.read_block_content()?;
                    self.expect_end_object()?;
                    let pointer_size = self.trace.pointer_size.clamp(4, 8) as usize;
                    parse_stack_block(&content, pointer_size, &mut self.stacks)?;
                }
                "SPBlock" => {
                    self.sequence_point_seen = true;
                    self.skip_block()?;
                    self.expect_end_object()?;
                }
                // Stacks carry nothing a counter view needs. Blocks are length-prefixed, so
                // skipping anything unrecognised is safe.
                _ => {
                    self.skip_block()?;
                    self.expect_end_object()?;
                }
            }
        }
    }

    fn expect_end_object(&mut self) -> Result<()> {
        fastserial::expect_tag(&mut self.stream, tag::END_OBJECT, "end of block")
    }

    /// Read a block's size, skip its alignment padding, and return its content.
    ///
    /// Padding is computed from the *absolute* stream offset after the size field, which is why
    /// the stream reader tracks position.
    fn read_block_content(&mut self) -> Result<Vec<u8>> {
        let size = self.block_size()?;
        self.skip_padding()?;
        self.stream.read_exact_bytes(size)
    }

    fn skip_block(&mut self) -> Result<()> {
        let size = self.block_size()?;
        self.skip_padding()?;
        self.stream.skip(size)
    }

    fn block_size(&mut self) -> Result<usize> {
        let size = self.stream.i32()?;
        if !(0..=MAX_BLOCK_SIZE).contains(&size) {
            return Err(ParseError::Unexpected {
                what: "block size",
                got: size.to_string(),
            });
        }
        Ok(size as usize)
    }

    fn skip_padding(&mut self) -> Result<()> {
        let padding = (4 - (self.stream.position() & 3)) & 3;
        self.stream.skip(padding as usize)
    }
}

fn parse_trace(body: &[u8]) -> Result<TraceInfo> {
    let mut r = Reader::new(body);
    let mut sync_time_utc = [0i16; 8];
    for part in &mut sync_time_utc {
        *part = r.i16()?;
    }
    Ok(TraceInfo {
        sync_time_utc,
        sync_time_qpc: r.i64()?,
        qpc_frequency: r.i64()?,
        pointer_size: r.i32()?,
        process_id: r.i32()?,
        processor_count: r.i32()?,
        sampling_rate: r.i32()?,
    })
}

/// A StackBlock: `FirstId:u32, Count:u32`, then per stack a `u32` byte length followed by that
/// many bytes of instruction pointers. Ids run consecutively from `FirstId`.
fn parse_stack_block(content: &[u8], pointer_size: usize, out: &mut StackTable) -> Result<()> {
    let mut r = Reader::new(content);
    let first_id = r.u32()?;
    let count = r.u32()?;

    for index in 0..count {
        let size_bytes = r.u32()? as usize;
        if !size_bytes.is_multiple_of(pointer_size) {
            return Err(ParseError::Unexpected {
                what: "stack size",
                got: format!("{size_bytes} bytes is not a multiple of the {pointer_size}-byte pointer"),
            });
        }

        let bytes = r.bytes(size_bytes, "stack frames")?;
        let frames: Vec<u64> = bytes
            .chunks_exact(pointer_size)
            .map(|chunk| {
                let mut buf = [0u8; 8];
                buf[..pointer_size].copy_from_slice(chunk);
                u64::from_le_bytes(buf)
            })
            .collect();

        out.current.insert(first_id.wrapping_add(index), frames);
    }
    Ok(())
}

/// Parse the rows of an Event or Metadata block. Header state is per-block, so it starts zeroed.
fn parse_event_rows(content: &[u8]) -> Result<Vec<RawEvent>> {
    let mut r = Reader::new(content);

    let header_size = r.u16()?;
    if header_size < MIN_BLOCK_HEADER_SIZE {
        return Err(ParseError::Unexpected {
            what: "block header size",
            got: header_size.to_string(),
        });
    }
    let flags = r.u16()?;
    // HeaderSize counts itself and Flags, both already consumed.
    r.skip(header_size as usize - 4, "block header remainder")?;

    let compressed = flags & FLAG_HEADER_COMPRESSION != 0;
    let mut header = EventHeader::default();
    let mut events = Vec::new();

    while !r.is_empty() {
        if compressed {
            header.read_compressed(&mut r)?;
        } else {
            header.read_uncompressed(&mut r)?;
        }
        let payload = r.bytes(header.payload_size as usize, "event payload")?;
        events.push(RawEvent {
            metadata_id: header.metadata_id,
            timestamp: header.timestamp,
            stack_id: header.stack_id,
            payload: payload.to_vec(),
        });
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nettrace::fastserial::{NETTRACE_MAGIC, SERIALIZER_HEADER};
    use crate::nettrace::metadata::TypeCode;
    use std::io::Cursor;

    /// Assembles a nettrace stream the way the runtime does, so the parser is exercised against
    /// real framing rather than a convenient approximation.
    #[derive(Default)]
    struct StreamBuilder {
        buf: Vec<u8>,
    }

    impl StreamBuilder {
        fn new() -> Self {
            let mut buf = NETTRACE_MAGIC.to_vec();
            buf.extend_from_slice(&20i32.to_le_bytes());
            buf.extend_from_slice(SERIALIZER_HEADER);
            Self { buf }
        }

        fn object_header(&mut self, type_name: &str) -> &mut Self {
            self.buf.extend_from_slice(&[
                tag::BEGIN_PRIVATE_OBJECT,
                tag::BEGIN_PRIVATE_OBJECT,
                tag::NULL_REFERENCE,
            ]);
            self.buf.extend_from_slice(&4i32.to_le_bytes());
            self.buf.extend_from_slice(&4i32.to_le_bytes());
            self.buf.extend_from_slice(&(type_name.len() as i32).to_le_bytes());
            self.buf.extend_from_slice(type_name.as_bytes());
            self.buf.push(tag::END_OBJECT);
            self
        }

        fn trace(&mut self, qpc_frequency: i64, process_id: i32) -> &mut Self {
            self.object_header("Trace");
            for part in [2026i16, 8, 0, 16, 12, 0, 0, 0] {
                self.buf.extend_from_slice(&part.to_le_bytes());
            }
            self.buf.extend_from_slice(&0i64.to_le_bytes()); // sync time qpc
            self.buf.extend_from_slice(&qpc_frequency.to_le_bytes());
            self.buf.extend_from_slice(&8i32.to_le_bytes()); // pointer size
            self.buf.extend_from_slice(&process_id.to_le_bytes());
            self.buf.extend_from_slice(&10i32.to_le_bytes()); // processors
            self.buf.extend_from_slice(&0i32.to_le_bytes()); // sampling rate
            self.buf.push(tag::END_OBJECT);
            self
        }

        /// A block whose content is the standard 20-byte header plus the given rows.
        fn block(&mut self, type_name: &str, rows: &[u8]) -> &mut Self {
            self.object_header(type_name);

            let mut content = Vec::new();
            content.extend_from_slice(&20u16.to_le_bytes()); // header size
            content.extend_from_slice(&1u16.to_le_bytes()); // flags: compressed
            content.extend_from_slice(&0i64.to_le_bytes()); // min timestamp
            content.extend_from_slice(&0i64.to_le_bytes()); // max timestamp
            content.extend_from_slice(rows);

            self.buf.extend_from_slice(&(content.len() as i32).to_le_bytes());
            // Alignment padding is relative to the absolute stream offset.
            let padding = (4 - (self.buf.len() & 3)) & 3;
            self.buf.extend(std::iter::repeat_n(0u8, padding));
            self.buf.extend_from_slice(&content);
            self.buf.push(tag::END_OBJECT);
            self
        }

        fn end(&mut self) -> Vec<u8> {
            self.buf.push(tag::NULL_REFERENCE);
            self.buf.clone()
        }
    }

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

    /// A compressed event row carrying metadata id, timestamp delta and payload.
    fn event_row(metadata_id: u32, timestamp_delta: u64, payload: &[u8]) -> Vec<u8> {
        let mut row = vec![0b1000_0001u8]; // MetadataId | PayloadSize
        row.extend(varint(u64::from(metadata_id)));
        row.extend(varint(timestamp_delta));
        row.extend(varint(payload.len() as u64));
        row.extend_from_slice(payload);
        row
    }

    fn wstr(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in s.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn metadata_record(id: u32, provider: &str, event: &str) -> Vec<u8> {
        let mut out = id.to_le_bytes().to_vec();
        out.extend(wstr(provider));
        out.extend_from_slice(&0i32.to_le_bytes()); // event id
        out.extend(wstr(event));
        out.extend_from_slice(&0i64.to_le_bytes()); // keywords
        out.extend_from_slice(&0i32.to_le_bytes()); // version
        out.extend_from_slice(&0i32.to_le_bytes()); // level
        out.extend_from_slice(&1i32.to_le_bytes()); // one field
        out.extend_from_slice(&(TypeCode::Int32 as i32).to_le_bytes());
        out.extend(wstr("Value"));
        out
    }

    #[test]
    fn parses_the_trace_object() {
        let bytes = StreamBuilder::new().trace(1_000_000_000, 77686).end();
        let mut parser = NettraceParser::new(Cursor::new(bytes)).unwrap();
        assert!(parser.next_events().unwrap().is_none());

        let trace = parser.trace_info();
        assert_eq!(trace.process_id, 77686);
        assert_eq!(trace.qpc_frequency, 1_000_000_000);
        assert_eq!(trace.pointer_size, 8);
        assert_eq!(trace.sync_time_utc[0], 2026);
    }

    #[test]
    fn trace_object_has_no_size_prefix_so_following_blocks_still_align() {
        // If the Trace body were treated as length-prefixed, this EventBlock would desync.
        let mut rows = event_row(1, 100, &[1, 0, 0, 0]);
        rows.extend(event_row(1, 50, &[2, 0, 0, 0]));

        let bytes = StreamBuilder::new()
            .trace(1_000_000_000, 1)
            .block("EventBlock", &rows)
            .end();

        let mut parser = NettraceParser::new(Cursor::new(bytes)).unwrap();
        let events = parser.next_events().unwrap().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].timestamp, 100);
        assert_eq!(events[1].timestamp, 150);
    }

    #[test]
    fn metadata_blocks_register_rather_than_yield() {
        let record = metadata_record(3, "System.Runtime", "EventCounters");
        let metadata_rows = event_row(0, 10, &record);
        let event_rows = event_row(3, 20, &[7, 0, 0, 0]);

        let bytes = StreamBuilder::new()
            .trace(1_000_000_000, 1)
            .block("MetadataBlock", &metadata_rows)
            .block("EventBlock", &event_rows)
            .end();

        let mut parser = NettraceParser::new(Cursor::new(bytes)).unwrap();
        let events = parser.next_events().unwrap().unwrap();

        assert_eq!(events.len(), 1, "only the event block yields events");
        assert_eq!(events[0].metadata_id, 3);

        let md = parser.metadata().get(3).expect("metadata registered");
        assert_eq!(md.provider_name, "System.Runtime");
        assert_eq!(md.event_name, "EventCounters");
    }

    #[test]
    fn stack_and_sequence_point_blocks_are_skipped() {
        let bytes = StreamBuilder::new()
            .trace(1_000_000_000, 1)
            .block("StackBlock", &[0xAB; 12])
            .block("SPBlock", &[0xCD; 8])
            .block("EventBlock", &event_row(1, 5, &[9, 0, 0, 0]))
            .end();

        let mut parser = NettraceParser::new(Cursor::new(bytes)).unwrap();
        let events = parser.next_events().unwrap().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].timestamp, 5);
    }

    #[test]
    fn end_of_stream_yields_none() {
        let bytes = StreamBuilder::new().trace(1_000_000_000, 1).end();
        let mut parser = NettraceParser::new(Cursor::new(bytes)).unwrap();
        assert!(parser.next_events().unwrap().is_none());
        assert!(parser.next_events().unwrap().is_none(), "stays finished");
    }

    #[test]
    fn header_state_resets_between_blocks() {
        // Both blocks start their timestamps from zero.
        let bytes = StreamBuilder::new()
            .trace(1_000_000_000, 1)
            .block("EventBlock", &event_row(1, 1_000, &[1, 0, 0, 0]))
            .block("EventBlock", &event_row(1, 7, &[2, 0, 0, 0]))
            .end();

        let mut parser = NettraceParser::new(Cursor::new(bytes)).unwrap();
        let first = parser.next_events().unwrap().unwrap();
        let second = parser.next_events().unwrap().unwrap();
        assert_eq!(first[0].timestamp, 1_000);
        assert_eq!(second[0].timestamp, 7, "not 1007");
    }

    /// A StackBlock body: FirstId, Count, then per stack a byte length and that many bytes.
    fn stack_block(first_id: u32, stacks: &[&[u64]]) -> Vec<u8> {
        let mut out = first_id.to_le_bytes().to_vec();
        out.extend_from_slice(&(stacks.len() as u32).to_le_bytes());
        for frames in stacks {
            out.extend_from_slice(&((frames.len() * 8) as u32).to_le_bytes());
            for frame in *frames {
                out.extend_from_slice(&frame.to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn stack_blocks_assign_consecutive_ids_from_first_id() {
        let mut table = StackTable::default();
        let block = stack_block(10, &[&[0xAA, 0xBB], &[0xCC]]);
        parse_stack_block(&block, 8, &mut table).unwrap();

        assert_eq!(table.get(10).unwrap(), &[0xAA, 0xBB]);
        assert_eq!(table.get(11).unwrap(), &[0xCC]);
        assert!(table.get(12).is_none());
    }

    #[test]
    fn a_stack_size_that_is_not_a_whole_number_of_pointers_is_rejected() {
        let mut table = StackTable::default();
        // 12 bytes cannot be 64-bit frames.
        let mut block = 0u32.to_le_bytes().to_vec();
        block.extend_from_slice(&1u32.to_le_bytes());
        block.extend_from_slice(&12u32.to_le_bytes());
        block.extend_from_slice(&[0u8; 12]);

        assert!(parse_stack_block(&block, 8, &mut table).is_err());
    }

    #[test]
    fn rotating_keeps_the_previous_generation_reachable() {
        // Samples referencing a stack defined just before a sequence point must still resolve.
        let mut table = StackTable::default();
        parse_stack_block(&stack_block(1, &[&[0x1111]]), 8, &mut table).unwrap();

        table.rotate();
        assert_eq!(table.get(1).unwrap(), &[0x1111], "still reachable one generation back");

        parse_stack_block(&stack_block(2, &[&[0x2222]]), 8, &mut table).unwrap();
        assert_eq!(table.get(1).unwrap(), &[0x1111]);
        assert_eq!(table.get(2).unwrap(), &[0x2222]);
    }

    #[test]
    fn a_recycled_id_resolves_to_the_newer_stack() {
        // Ids are reused after a sequence point; the current generation must win.
        let mut table = StackTable::default();
        parse_stack_block(&stack_block(1, &[&[0xDEAD]]), 8, &mut table).unwrap();
        table.rotate();
        parse_stack_block(&stack_block(1, &[&[0xBEEF]]), 8, &mut table).unwrap();

        assert_eq!(table.get(1).unwrap(), &[0xBEEF]);
    }

    #[test]
    fn two_rotations_drop_the_oldest_generation() {
        let mut table = StackTable::default();
        parse_stack_block(&stack_block(1, &[&[0x1111]]), 8, &mut table).unwrap();
        table.rotate();
        table.rotate();
        assert!(table.get(1).is_none(), "bounded memory, not unbounded history");
        assert!(table.is_empty());
    }

    #[test]
    fn thirty_two_bit_frames_are_read_at_pointer_width() {
        let mut table = StackTable::default();
        let mut block = 0u32.to_le_bytes().to_vec();
        block.extend_from_slice(&1u32.to_le_bytes());
        block.extend_from_slice(&8u32.to_le_bytes());
        block.extend_from_slice(&0x1111_2222u32.to_le_bytes());
        block.extend_from_slice(&0x3333_4444u32.to_le_bytes());

        parse_stack_block(&block, 4, &mut table).unwrap();
        assert_eq!(table.get(0).unwrap(), &[0x1111_2222, 0x3333_4444]);
    }

    #[test]
    fn elapsed_seconds_use_the_qpc_frequency() {
        let trace = TraceInfo { qpc_frequency: 1_000_000_000, ..Default::default() };
        assert_eq!(trace.elapsed_secs(0, 1_500_000_000), 1.5);
    }

    #[test]
    fn a_truncated_stream_errors_rather_than_hanging() {
        let full = StreamBuilder::new()
            .trace(1_000_000_000, 1)
            .block("EventBlock", &event_row(1, 5, &[1, 0, 0, 0]))
            .end();
        let truncated = &full[..full.len() - 10];

        let mut parser = NettraceParser::new(Cursor::new(truncated.to_vec())).unwrap();
        assert!(parser.next_events().is_err());
    }
}
