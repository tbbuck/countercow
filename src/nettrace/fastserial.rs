//! The FastSerialization envelope that nettrace V4 wraps its blocks in.

use std::io::Read;

use super::reader::{ParseError, Result, StreamReader};

pub const NETTRACE_MAGIC: &[u8; 8] = b"Nettrace";
pub const SERIALIZER_HEADER: &[u8; 20] = b"!FastSerialization.1";

pub mod tag {
    pub const NULL_REFERENCE: u8 = 1;
    pub const BEGIN_PRIVATE_OBJECT: u8 = 5;
    pub const END_OBJECT: u8 = 6;
}

#[derive(Debug, Clone)]
pub struct ObjectHeader {
    pub type_name: String,
    pub version: i32,
    pub min_reader_version: i32,
}

/// Read and validate the 32-byte stream preamble.
pub fn read_preamble<R: Read>(r: &mut StreamReader<R>) -> Result<()> {
    let magic = r.read_exact_bytes(NETTRACE_MAGIC.len())?;
    if magic != NETTRACE_MAGIC {
        return Err(ParseError::Unexpected {
            what: "stream magic",
            got: String::from_utf8_lossy(&magic).into_owned(),
        });
    }

    let len = r.i32()?;
    if len != SERIALIZER_HEADER.len() as i32 {
        return Err(ParseError::Unexpected {
            what: "serializer header length",
            got: len.to_string(),
        });
    }

    let header = r.read_exact_bytes(len as usize)?;
    if header != SERIALIZER_HEADER {
        return Err(ParseError::Unexpected {
            what: "serializer header",
            got: String::from_utf8_lossy(&header).into_owned(),
        });
    }
    Ok(())
}

/// Read the header introducing the next object, or `None` at end of stream.
///
/// End of stream is a bare NullReference tag where an object would begin.
pub fn read_object_header<R: Read>(r: &mut StreamReader<R>) -> Result<Option<ObjectHeader>> {
    let tag = r.u8()?;
    match tag {
        tag::NULL_REFERENCE => return Ok(None),
        tag::BEGIN_PRIVATE_OBJECT => {}
        other => {
            return Err(ParseError::Unexpected {
                what: "object tag",
                got: format!("0x{other:02x}"),
            })
        }
    }

    // The object's type is itself a serialized object, whose own type is null by convention.
    expect_tag(r, tag::BEGIN_PRIVATE_OBJECT, "serialization type tag")?;
    expect_tag(r, tag::NULL_REFERENCE, "serialization type's type tag")?;

    let version = r.i32()?;
    let min_reader_version = r.i32()?;
    let name_len = r.i32()?;
    if !(0..=1024).contains(&name_len) {
        return Err(ParseError::Unexpected {
            what: "type name length",
            got: name_len.to_string(),
        });
    }
    // The type name is UTF-8 and is *not* NUL-terminated.
    let name_bytes = r.read_exact_bytes(name_len as usize)?;
    let type_name = String::from_utf8_lossy(&name_bytes).into_owned();

    expect_tag(r, tag::END_OBJECT, "end of serialization type")?;

    Ok(Some(ObjectHeader { type_name, version, min_reader_version }))
}

pub fn expect_tag<R: Read>(r: &mut StreamReader<R>, expected: u8, what: &'static str) -> Result<()> {
    let got = r.u8()?;
    if got != expected {
        return Err(ParseError::Unexpected { what, got: format!("0x{got:02x}") });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn preamble() -> Vec<u8> {
        let mut out = NETTRACE_MAGIC.to_vec();
        out.extend_from_slice(&20i32.to_le_bytes());
        out.extend_from_slice(SERIALIZER_HEADER);
        out
    }

    #[test]
    fn accepts_a_valid_preamble_and_consumes_exactly_32_bytes() {
        let mut r = StreamReader::new(Cursor::new(preamble()));
        read_preamble(&mut r).unwrap();
        assert_eq!(r.position(), 32);
    }

    #[test]
    fn rejects_a_foreign_stream() {
        let mut bytes = b"NotTrace".to_vec();
        bytes.extend_from_slice(&20i32.to_le_bytes());
        bytes.extend_from_slice(SERIALIZER_HEADER);
        let mut r = StreamReader::new(Cursor::new(bytes));
        assert!(read_preamble(&mut r).is_err());
    }

    fn object_header(type_name: &str, version: i32) -> Vec<u8> {
        let mut out = vec![tag::BEGIN_PRIVATE_OBJECT, tag::BEGIN_PRIVATE_OBJECT, tag::NULL_REFERENCE];
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&(type_name.len() as i32).to_le_bytes());
        out.extend_from_slice(type_name.as_bytes());
        out.push(tag::END_OBJECT);
        out
    }

    #[test]
    fn reads_a_trace_object_header() {
        let mut r = StreamReader::new(Cursor::new(object_header("Trace", 4)));
        let header = read_object_header(&mut r).unwrap().unwrap();
        assert_eq!(header.type_name, "Trace");
        assert_eq!(header.version, 4);
        assert_eq!(header.min_reader_version, 4);
    }

    #[test]
    fn null_reference_signals_end_of_stream() {
        let mut r = StreamReader::new(Cursor::new(vec![tag::NULL_REFERENCE]));
        assert!(read_object_header(&mut r).unwrap().is_none());
    }

    #[test]
    fn a_stray_tag_is_an_error() {
        let mut r = StreamReader::new(Cursor::new(vec![0x42]));
        assert!(read_object_header(&mut r).is_err());
    }

    #[test]
    fn full_preamble_then_trace_header_lands_at_the_documented_offset() {
        let mut bytes = preamble();
        bytes.extend(object_header("Trace", 4));
        let mut r = StreamReader::new(Cursor::new(bytes));
        read_preamble(&mut r).unwrap();
        read_object_header(&mut r).unwrap().unwrap();
        // 32-byte preamble + 3 tags + 3 i32 + 5 name bytes + 1 end tag = 53.
        assert_eq!(r.position(), 53);
    }
}
