//! Event metadata records: what an event is called, and the shape of its payload.
//!
//! A metadata record carries a header, then a field list in one of two encodings. The V1 list is
//! inline; a V5 "tag" can supersede it with a V2 list. **The field order is inverted between
//! them** — V1 is type-then-name, V2 is size-then-name-then-type — which is an easy way to
//! produce a parser that reads plausible garbage rather than failing.

use std::collections::HashMap;

use super::reader::{ParseError, Reader, Result};

/// Guards against malformed input driving unbounded recursion through nested Object types.
const MAX_TYPE_DEPTH: usize = 16;

/// Field types, as `System.TypeCode` plus two .NET extensions (Guid, Array).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeCode {
    Object = 1,
    Boolean = 3,
    Char = 4,
    SByte = 5,
    Byte = 6,
    Int16 = 7,
    UInt16 = 8,
    Int32 = 9,
    UInt32 = 10,
    Int64 = 11,
    UInt64 = 12,
    Single = 13,
    Double = 14,
    Decimal = 15,
    DateTime = 16,
    Guid = 17,
    String = 18,
    Array = 19,
}

impl TypeCode {
    pub fn from_i32(v: i32) -> Option<Self> {
        Some(match v {
            1 => TypeCode::Object,
            3 => TypeCode::Boolean,
            4 => TypeCode::Char,
            5 => TypeCode::SByte,
            6 => TypeCode::Byte,
            7 => TypeCode::Int16,
            8 => TypeCode::UInt16,
            9 => TypeCode::Int32,
            10 => TypeCode::UInt32,
            11 => TypeCode::Int64,
            12 => TypeCode::UInt64,
            13 => TypeCode::Single,
            14 => TypeCode::Double,
            15 => TypeCode::Decimal,
            16 => TypeCode::DateTime,
            17 => TypeCode::Guid,
            18 => TypeCode::String,
            19 => TypeCode::Array,
            _ => return None,
        })
    }

    /// Fixed wire width, or `None` for variable-length types.
    pub fn fixed_size(self) -> Option<usize> {
        Some(match self {
            TypeCode::Boolean => 4, // a 4-byte int, not one byte
            TypeCode::Char | TypeCode::Int16 | TypeCode::UInt16 => 2,
            TypeCode::SByte | TypeCode::Byte => 1,
            TypeCode::Int32 | TypeCode::UInt32 | TypeCode::Single => 4,
            TypeCode::Int64 | TypeCode::UInt64 | TypeCode::Double => 8,
            TypeCode::Decimal | TypeCode::Guid => 16,
            TypeCode::DateTime => 16, // 8 x i16
            TypeCode::Object | TypeCode::String | TypeCode::Array => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub enum FieldType {
    Scalar(TypeCode),
    /// A nested struct. Contributes no bytes of its own; its fields are inlined.
    Object(Vec<Field>),
    Array(Box<FieldType>),
    /// A type code we do not model. Its presence makes the payload undecodable from here on,
    /// which the payload decoder reports rather than guessing at.
    Unknown(i32),
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: FieldType,
}

#[derive(Debug, Clone)]
pub struct EventMetadata {
    pub metadata_id: u32,
    pub provider_name: String,
    pub event_id: i32,
    pub event_name: String,
    pub keywords: u64,
    pub version: i32,
    pub level: i32,
    pub fields: Vec<Field>,
}

/// Registry of metadata records seen so far, keyed by the id events reference.
#[derive(Debug, Default)]
pub struct MetadataStore {
    by_id: HashMap<u32, EventMetadata>,
}

impl MetadataStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, metadata: EventMetadata) {
        self.by_id.insert(metadata.metadata_id, metadata);
    }

    pub fn get(&self, metadata_id: u32) -> Option<&EventMetadata> {
        self.by_id.get(&metadata_id)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &EventMetadata> {
        self.by_id.values()
    }
}

const TAG_OPCODE: u8 = 1;
const TAG_PARAMETER_PAYLOAD_V2: u8 = 2;

/// Parse one metadata record, which arrives as the payload of an event inside a MetadataBlock.
pub fn parse(payload: &[u8]) -> Result<EventMetadata> {
    let mut r = Reader::new(payload);

    let metadata_id = r.u32()?;
    let provider_name = r.utf16_nul_string()?;
    let event_id = r.i32()?;
    let event_name = r.utf16_nul_string()?;
    let keywords = r.u64()?;
    let version = r.i32()?;
    let level = r.i32()?;

    let mut fields = if r.is_empty() {
        // No parameter list at all: the event has no payload.
        Vec::new()
    } else {
        read_fields_v1(&mut r, 0)?
    };

    // V5 tags follow the V1 list. Parse them whenever bytes remain rather than gating on a
    // version number: the Trace object reports version 4 even on runtimes that emit V5 tags.
    while r.remaining() > 4 {
        let tag_payload_bytes = r.i32()?;
        if tag_payload_bytes < 0 {
            return Err(ParseError::Unexpected {
                what: "metadata tag length",
                got: tag_payload_bytes.to_string(),
            });
        }
        let kind = r.u8()?;
        let body = r.bytes(tag_payload_bytes as usize, "metadata tag payload")?;

        match kind {
            TAG_OPCODE => { /* a single opcode byte; not needed for counters */ }
            TAG_PARAMETER_PAYLOAD_V2 => {
                // When a V2 list is present it is authoritative and the V1 list must be empty.
                fields = read_fields_v2(&mut Reader::new(body), 0)?;
            }
            _ => { /* unknown tag: skipped by construction */ }
        }
    }

    Ok(EventMetadata {
        metadata_id,
        provider_name,
        event_id,
        event_name,
        keywords,
        version,
        level,
        fields,
    })
}

/// V1 field list: a count, then per field the *type first*, then the name.
fn read_fields_v1(r: &mut Reader<'_>, depth: usize) -> Result<Vec<Field>> {
    if depth > MAX_TYPE_DEPTH {
        return Err(ParseError::Unexpected {
            what: "metadata nesting depth",
            got: depth.to_string(),
        });
    }

    let count = r.i32()?;
    if count < 0 {
        return Err(ParseError::Unexpected {
            what: "V1 field count",
            got: count.to_string(),
        });
    }

    let mut fields = Vec::with_capacity(count.min(1024) as usize);
    for _ in 0..count {
        let ty = read_type_v1(r, depth)?;
        let name = r.utf16_nul_string()?;
        fields.push(Field { name, ty });
    }
    Ok(fields)
}

fn read_type_v1(r: &mut Reader<'_>, depth: usize) -> Result<FieldType> {
    let raw = r.i32()?;
    Ok(match TypeCode::from_i32(raw) {
        Some(TypeCode::Object) => FieldType::Object(read_fields_v1(r, depth + 1)?),
        Some(code) => FieldType::Scalar(code),
        None => FieldType::Unknown(raw),
    })
}

/// V2 field list: a count, then per field a *size prefix*, the name, and only then the type.
///
/// The size is inclusive of itself, so slicing to `size - 4` both bounds the field and absorbs
/// its trailing padding.
fn read_fields_v2(r: &mut Reader<'_>, depth: usize) -> Result<Vec<Field>> {
    if depth > MAX_TYPE_DEPTH {
        return Err(ParseError::Unexpected {
            what: "metadata nesting depth",
            got: depth.to_string(),
        });
    }

    let count = r.i32()?;
    if count < 0 {
        return Err(ParseError::Unexpected {
            what: "V2 field count",
            got: count.to_string(),
        });
    }

    let mut fields = Vec::with_capacity(count.min(1024) as usize);
    for _ in 0..count {
        let size = r.i32()?;
        if size < 4 {
            return Err(ParseError::Unexpected {
                what: "V2 field size",
                got: size.to_string(),
            });
        }
        let body = r.bytes(size as usize - 4, "V2 field body")?;
        let mut field_reader = Reader::new(body);

        let name = field_reader.utf16_nul_string()?;
        let ty = read_type_v2(&mut field_reader, depth)?;
        fields.push(Field { name, ty });
    }
    Ok(fields)
}

fn read_type_v2(r: &mut Reader<'_>, depth: usize) -> Result<FieldType> {
    let raw = r.i32()?;
    Ok(match TypeCode::from_i32(raw) {
        Some(TypeCode::Array) => FieldType::Array(Box::new(read_type_v2(r, depth + 1)?)),
        Some(TypeCode::Object) => FieldType::Object(read_fields_v2(r, depth + 1)?),
        Some(code) => FieldType::Scalar(code),
        None => FieldType::Unknown(raw),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds metadata payloads the way the runtime does, so the tests exercise real layouts.
    #[derive(Default)]
    struct MetadataBuilder {
        buf: Vec<u8>,
    }

    impl MetadataBuilder {
        fn i32(&mut self, v: i32) -> &mut Self {
            self.buf.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn u32(&mut self, v: u32) -> &mut Self {
            self.buf.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn i64(&mut self, v: i64) -> &mut Self {
            self.buf.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn u8(&mut self, v: u8) -> &mut Self {
            self.buf.push(v);
            self
        }
        fn wstr(&mut self, s: &str) -> &mut Self {
            for unit in s.encode_utf16() {
                self.buf.extend_from_slice(&unit.to_le_bytes());
            }
            self.buf.extend_from_slice(&[0, 0]);
            self
        }
        fn header(&mut self, id: u32, provider: &str, event: &str) -> &mut Self {
            self.u32(id).wstr(provider).i32(0).wstr(event).i64(0).i32(1).i32(4)
        }
    }

    #[test]
    fn parses_header_and_flat_v1_fields() {
        let mut b = MetadataBuilder::default();
        b.header(7, "System.Runtime", "EventCounters")
            .i32(2) // field count
            .i32(TypeCode::Int32 as i32)
            .wstr("Count")
            .i32(TypeCode::String as i32)
            .wstr("Name");

        let md = parse(&b.buf).unwrap();
        assert_eq!(md.metadata_id, 7);
        assert_eq!(md.provider_name, "System.Runtime");
        assert_eq!(md.event_name, "EventCounters");
        assert_eq!(md.version, 1);
        assert_eq!(md.level, 4);
        assert_eq!(md.fields.len(), 2);
        assert_eq!(md.fields[0].name, "Count");
        assert!(matches!(md.fields[0].ty, FieldType::Scalar(TypeCode::Int32)));
        assert_eq!(md.fields[1].name, "Name");
    }

    #[test]
    fn parses_nested_v1_objects() {
        // The real EventCounters shape: an unnamed outer struct wrapping a "Payload" struct.
        let mut b = MetadataBuilder::default();
        b.header(3, "System.Runtime", "EventCounters")
            .i32(1)
            .i32(TypeCode::Object as i32)
            .i32(1)
            .i32(TypeCode::Object as i32)
            .i32(2)
            .i32(TypeCode::String as i32)
            .wstr("Name")
            .i32(TypeCode::Double as i32)
            .wstr("Mean")
            .wstr("Payload")
            .wstr("");

        let md = parse(&b.buf).unwrap();
        assert_eq!(md.fields.len(), 1);
        assert_eq!(md.fields[0].name, "");
        let FieldType::Object(outer) = &md.fields[0].ty else {
            panic!("expected an outer object");
        };
        assert_eq!(outer[0].name, "Payload");
        let FieldType::Object(inner) = &outer[0].ty else {
            panic!("expected a Payload object");
        };
        assert_eq!(inner.len(), 2);
        assert_eq!(inner[0].name, "Name");
        assert_eq!(inner[1].name, "Mean");
    }

    #[test]
    fn no_field_list_means_no_payload() {
        let mut b = MetadataBuilder::default();
        b.header(1, "P", "E");
        let md = parse(&b.buf).unwrap();
        assert!(md.fields.is_empty());
    }

    #[test]
    fn opcode_tag_is_skipped_without_disturbing_fields() {
        let mut b = MetadataBuilder::default();
        b.header(4, "P", "E")
            .i32(1)
            .i32(TypeCode::Int32 as i32)
            .wstr("N")
            // tag: 1 payload byte, kind Opcode
            .i32(1)
            .u8(TAG_OPCODE)
            .u8(9);

        let md = parse(&b.buf).unwrap();
        assert_eq!(md.fields.len(), 1);
        assert_eq!(md.fields[0].name, "N");
    }

    #[test]
    fn v2_tag_supersedes_the_v1_list_and_inverts_field_order() {
        // V2 field body: name first, then type — the opposite of V1.
        let mut field = MetadataBuilder::default();
        field.wstr("Duration").i32(TypeCode::Double as i32);
        let field_size = 4 + field.buf.len() as i32;

        let mut v2 = MetadataBuilder::default();
        v2.i32(1).i32(field_size);
        v2.buf.extend_from_slice(&field.buf);

        let mut b = MetadataBuilder::default();
        b.header(5, "P", "E")
            // A V1 list that must be discarded once the V2 tag appears.
            .i32(1)
            .i32(TypeCode::Int32 as i32)
            .wstr("stale");
        b.i32(v2.buf.len() as i32).u8(TAG_PARAMETER_PAYLOAD_V2);
        b.buf.extend_from_slice(&v2.buf);

        let md = parse(&b.buf).unwrap();
        assert_eq!(md.fields.len(), 1);
        assert_eq!(md.fields[0].name, "Duration", "V2 list must win");
        assert!(matches!(md.fields[0].ty, FieldType::Scalar(TypeCode::Double)));
    }

    #[test]
    fn v2_field_padding_is_absorbed() {
        // Declare a field larger than its contents; the slack is padding.
        let mut field = MetadataBuilder::default();
        field.wstr("X").i32(TypeCode::Int32 as i32);
        let padded_size = 4 + field.buf.len() as i32 + 3;

        let mut v2 = MetadataBuilder::default();
        v2.i32(1).i32(padded_size);
        v2.buf.extend_from_slice(&field.buf);
        v2.buf.extend_from_slice(&[0, 0, 0]);
        // A second field proves the reader resumed at the right offset.
        let mut second = MetadataBuilder::default();
        second.wstr("Y").i32(TypeCode::Int64 as i32);
        v2.i32(4 + second.buf.len() as i32);
        v2.buf.extend_from_slice(&second.buf);
        // Fix the count now that there are two.
        v2.buf[0..4].copy_from_slice(&2i32.to_le_bytes());

        let mut b = MetadataBuilder::default();
        b.header(6, "P", "E").i32(0);
        b.i32(v2.buf.len() as i32).u8(TAG_PARAMETER_PAYLOAD_V2);
        b.buf.extend_from_slice(&v2.buf);

        let md = parse(&b.buf).unwrap();
        assert_eq!(md.fields.len(), 2);
        assert_eq!(md.fields[0].name, "X");
        assert_eq!(md.fields[1].name, "Y");
        assert!(matches!(md.fields[1].ty, FieldType::Scalar(TypeCode::Int64)));
    }

    #[test]
    fn unknown_type_code_is_preserved_not_guessed() {
        let mut b = MetadataBuilder::default();
        b.header(8, "P", "E").i32(1).i32(999).wstr("Odd");
        let md = parse(&b.buf).unwrap();
        assert!(matches!(md.fields[0].ty, FieldType::Unknown(999)));
    }

    #[test]
    fn deep_nesting_is_rejected_rather_than_overflowing_the_stack() {
        let mut b = MetadataBuilder::default();
        b.header(9, "P", "E");
        for _ in 0..64 {
            b.i32(1).i32(TypeCode::Object as i32);
        }
        b.i32(0);
        assert!(parse(&b.buf).is_err());
    }
}
