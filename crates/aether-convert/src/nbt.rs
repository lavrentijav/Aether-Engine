//! A small, allocation-light NBT reader.
//!
//! Named Binary Tag is Minecraft's big-endian tree format. This reader parses
//! the subset the Anvil chunk format uses (all tag types are handled) into an
//! owned [`Nbt`] tree with convenience accessors. It is read-only — the
//! converter never writes NBT back.

/// A parsed NBT value.
#[derive(Debug, Clone, PartialEq)]
pub enum Nbt {
    /// TAG_End is only used internally as a list/compound terminator.
    End,
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(Vec<Nbt>),
    Compound(Vec<(String, Nbt)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

/// Errors from NBT parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NbtError {
    /// Reached the end of the buffer mid-value.
    Truncated,
    /// Encountered a tag id that is not defined.
    BadTag(u8),
    /// A string held invalid UTF-8 (Java modified UTF-8 not fully supported).
    BadString,
    /// The root tag was not a compound, as the spec requires.
    RootNotCompound,
}

impl std::fmt::Display for NbtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NbtError::Truncated => f.write_str("NBT data truncated"),
            NbtError::BadTag(t) => write!(f, "unknown NBT tag id {t}"),
            NbtError::BadString => f.write_str("invalid NBT string"),
            NbtError::RootNotCompound => f.write_str("NBT root is not a compound"),
        }
    }
}

impl std::error::Error for NbtError {}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], NbtError> {
        let end = self.pos.checked_add(n).ok_or(NbtError::Truncated)?;
        let s = self.buf.get(self.pos..end).ok_or(NbtError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, NbtError> {
        Ok(self.take(1)?[0])
    }
    fn i16(&mut self) -> Result<i16, NbtError> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u16(&mut self) -> Result<u16, NbtError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, NbtError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, NbtError> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, NbtError> {
        Ok(f32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64, NbtError> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String, NbtError> {
        let len = self.u16()? as usize;
        let bytes = self.take(len)?;
        // Java uses modified UTF-8; plain UTF-8 covers all identifiers we read.
        std::str::from_utf8(bytes)
            .map(|s| s.to_owned())
            .map_err(|_| NbtError::BadString)
    }

    fn payload(&mut self, tag: u8) -> Result<Nbt, NbtError> {
        Ok(match tag {
            0 => Nbt::End,
            1 => Nbt::Byte(self.u8()? as i8),
            2 => Nbt::Short(self.i16()?),
            3 => Nbt::Int(self.i32()?),
            4 => Nbt::Long(self.i64()?),
            5 => Nbt::Float(self.f32()?),
            6 => Nbt::Double(self.f64()?),
            7 => {
                let len = self.i32()?.max(0) as usize;
                Nbt::ByteArray(self.take(len)?.iter().map(|&b| b as i8).collect())
            }
            8 => Nbt::String(self.string()?),
            9 => {
                let elem = self.u8()?;
                let len = self.i32()?.max(0) as usize;
                let mut items = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    items.push(self.payload(elem)?);
                }
                Nbt::List(items)
            }
            10 => {
                let mut fields = Vec::new();
                loop {
                    let child = self.u8()?;
                    if child == 0 {
                        break;
                    }
                    let name = self.string()?;
                    fields.push((name, self.payload(child)?));
                }
                Nbt::Compound(fields)
            }
            11 => {
                let len = self.i32()?.max(0) as usize;
                let mut v = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    v.push(self.i32()?);
                }
                Nbt::IntArray(v)
            }
            12 => {
                let len = self.i32()?.max(0) as usize;
                let mut v = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    v.push(self.i64()?);
                }
                Nbt::LongArray(v)
            }
            other => return Err(NbtError::BadTag(other)),
        })
    }
}

/// Parse an uncompressed NBT buffer whose root is a named compound.
pub fn parse(buf: &[u8]) -> Result<Nbt, NbtError> {
    let mut c = Cursor { buf, pos: 0 };
    let root_tag = c.u8()?;
    if root_tag != 10 {
        return Err(NbtError::RootNotCompound);
    }
    let _root_name = c.string()?;
    c.payload(10)
}

impl Nbt {
    /// Look up a field of a compound by name.
    pub fn get(&self, key: &str) -> Option<&Nbt> {
        match self {
            Nbt::Compound(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    /// As a `&str` if this is a string tag.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Nbt::String(s) => Some(s),
            _ => None,
        }
    }
    /// As an `i64` if this is any integer tag.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Nbt::Byte(v) => Some(*v as i64),
            Nbt::Short(v) => Some(*v as i64),
            Nbt::Int(v) => Some(*v as i64),
            Nbt::Long(v) => Some(*v),
            _ => None,
        }
    }
    /// As a list slice if this is a list (or compound-less) tag.
    pub fn as_list(&self) -> Option<&[Nbt]> {
        match self {
            Nbt::List(items) => Some(items),
            _ => None,
        }
    }
    /// As a `long[]` if this is a long-array tag.
    pub fn as_long_array(&self) -> Option<&[i64]> {
        match self {
            Nbt::LongArray(v) => Some(v),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build: compound{ "name": "hi", "n": 300i32, "list":[1i8,2i8] }
    fn sample() -> Vec<u8> {
        let mut b = Vec::new();
        b.push(10); // root compound
        b.extend_from_slice(&0u16.to_be_bytes()); // root name ""
                                                  // string field
        b.push(8);
        b.extend_from_slice(&4u16.to_be_bytes());
        b.extend_from_slice(b"name");
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(b"hi");
        // int field
        b.push(3);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(b"n");
        b.extend_from_slice(&300i32.to_be_bytes());
        // list of bytes
        b.push(9);
        b.extend_from_slice(&4u16.to_be_bytes());
        b.extend_from_slice(b"list");
        b.push(1); // element type byte
        b.extend_from_slice(&2i32.to_be_bytes());
        b.push(1);
        b.push(2);
        b.push(0); // end root
        b
    }

    #[test]
    fn parses_nested_values() {
        let nbt = parse(&sample()).unwrap();
        assert_eq!(nbt.get("name").and_then(Nbt::as_str), Some("hi"));
        assert_eq!(nbt.get("n").and_then(Nbt::as_i64), Some(300));
        let list = nbt.get("list").and_then(Nbt::as_list).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], Nbt::Byte(1));
    }

    #[test]
    fn rejects_non_compound_root_and_truncation() {
        assert_eq!(parse(&[3, 0, 0]), Err(NbtError::RootNotCompound));
        assert_eq!(parse(&[10, 0, 0, 3]), Err(NbtError::Truncated));
    }
}
