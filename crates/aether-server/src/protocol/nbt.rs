//! Minimal NBT **writer**.
//!
//! `aether-convert` already parses NBT (it reads Anvil regions), but nothing
//! in the workspace produced it. Modern protocol versions carry their
//! registries — dimension types, biomes, damage types — as NBT in the
//! configuration phase, so the 1.21 codec needs to emit it.
//!
//! Since 1.20.2 the network form of a root compound is **unnamed**: the tag
//! byte is followed straight by the payload, with no name string. That is the
//! `anonymousNbt` the protocol schema refers to, and [`Nbt::to_network`]
//! writes exactly that.

/// An NBT value.
///
/// All thirteen tag types are present even though the registries this server
/// currently sends use only some of them: a writer that silently cannot
/// represent part of the format is a trap for the next caller, and the tag
/// numbering only makes sense as a complete set.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    Str(String),
    /// A list; every element must share one tag type.
    List(Vec<Nbt>),
    Compound(Vec<(String, Nbt)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Nbt {
    /// The tag byte identifying this value's type.
    pub fn tag(&self) -> u8 {
        match self {
            Nbt::Byte(_) => 1,
            Nbt::Short(_) => 2,
            Nbt::Int(_) => 3,
            Nbt::Long(_) => 4,
            Nbt::Float(_) => 5,
            Nbt::Double(_) => 6,
            Nbt::ByteArray(_) => 7,
            Nbt::Str(_) => 8,
            Nbt::List(_) => 9,
            Nbt::Compound(_) => 10,
            Nbt::IntArray(_) => 11,
            Nbt::LongArray(_) => 12,
        }
    }

    /// Append just this value's payload (no tag byte, no name).
    fn write_payload(&self, out: &mut Vec<u8>) {
        match self {
            Nbt::Byte(v) => out.push(*v as u8),
            Nbt::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
            Nbt::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
            Nbt::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
            Nbt::Float(v) => out.extend_from_slice(&v.to_be_bytes()),
            Nbt::Double(v) => out.extend_from_slice(&v.to_be_bytes()),
            Nbt::ByteArray(b) => {
                out.extend_from_slice(&(b.len() as i32).to_be_bytes());
                out.extend_from_slice(b);
            }
            Nbt::Str(s) => write_string(out, s),
            Nbt::List(items) => {
                // An empty list still needs an element type; TAG_End is the
                // conventional filler and every vanilla parser accepts it.
                let elem = items.first().map(|n| n.tag()).unwrap_or(0);
                out.push(elem);
                out.extend_from_slice(&(items.len() as i32).to_be_bytes());
                for item in items {
                    debug_assert_eq!(item.tag(), elem, "heterogeneous NBT list");
                    item.write_payload(out);
                }
            }
            Nbt::Compound(fields) => {
                for (name, value) in fields {
                    out.push(value.tag());
                    write_string(out, name);
                    value.write_payload(out);
                }
                out.push(0); // TAG_End
            }
            Nbt::IntArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for i in v {
                    out.extend_from_slice(&i.to_be_bytes());
                }
            }
            Nbt::LongArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for i in v {
                    out.extend_from_slice(&i.to_be_bytes());
                }
            }
        }
    }

    /// Serialise in the modern network form: tag byte, then payload, with no
    /// root name.
    pub fn to_network(&self) -> Vec<u8> {
        let mut out = vec![self.tag()];
        self.write_payload(&mut out);
        out
    }
}

/// A length-prefixed modified-UTF-8 string (plain UTF-8 for our inputs).
fn write_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Build a compound from `(name, value)` pairs.
pub fn compound<const N: usize>(fields: [(&str, Nbt); N]) -> Nbt {
    Nbt::Compound(fields.map(|(k, v)| (k.to_string(), v)).into())
}

/// A string tag from a `&str`.
pub fn string(s: &str) -> Nbt {
    Nbt::Str(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_root_compound_has_no_name() {
        // Tag byte, then straight into fields — no 2-byte name length.
        let nbt = compound([("a", Nbt::Byte(1))]);
        let bytes = nbt.to_network();
        assert_eq!(bytes[0], 10, "root tag is TAG_Compound");
        assert_eq!(
            bytes[1], 1,
            "next byte is the first field's tag, not a name"
        );
        assert_eq!(&bytes[2..4], &[0, 1], "field name length");
        assert_eq!(&bytes[4..5], b"a");
        assert_eq!(bytes[5], 1, "field payload");
        assert_eq!(bytes[6], 0, "TAG_End");
    }

    #[test]
    fn scalars_are_big_endian() {
        assert_eq!(Nbt::Int(1).to_network(), vec![3, 0, 0, 0, 1]);
        assert_eq!(Nbt::Short(258).to_network(), vec![2, 1, 2]);
        assert_eq!(Nbt::Long(1).to_network(), vec![4, 0, 0, 0, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn empty_list_still_declares_an_element_type() {
        let bytes = Nbt::List(vec![]).to_network();
        assert_eq!(bytes, vec![9, 0, 0, 0, 0, 0], "TAG_End element type, len 0");
    }

    #[test]
    fn list_writes_element_type_once_then_bare_payloads() {
        let bytes = Nbt::List(vec![Nbt::Int(7), Nbt::Int(8)]).to_network();
        assert_eq!(bytes, vec![9, 3, 0, 0, 0, 2, 0, 0, 0, 7, 0, 0, 0, 8]);
    }

    /// The parser in `aether-convert` reads the classic *named* root form, so
    /// it cannot be used to round-trip the network form directly. Decode by
    /// hand instead, mirroring what a client does.
    #[test]
    fn nested_compound_round_trips_by_hand() {
        let nbt = compound([
            ("name", string("minecraft:overworld")),
            ("height", Nbt::Int(384)),
            ("inner", compound([("flag", Nbt::Byte(1))])),
        ]);
        let b = nbt.to_network();
        let mut i = 1usize; // skip root tag
        let mut seen = Vec::new();
        while b[i] != 0 {
            let tag = b[i];
            i += 1;
            let n = u16::from_be_bytes([b[i], b[i + 1]]) as usize;
            i += 2;
            let name = String::from_utf8(b[i..i + n].to_vec()).unwrap();
            i += n;
            match tag {
                8 => {
                    let l = u16::from_be_bytes([b[i], b[i + 1]]) as usize;
                    i += 2 + l;
                }
                3 => i += 4,
                10 => {
                    // one byte field + TAG_End, skipping its own header
                    let l = u16::from_be_bytes([b[i + 1], b[i + 2]]) as usize;
                    i += 1 + 2 + l + 1 + 1;
                }
                other => panic!("unexpected tag {other}"),
            }
            seen.push(name);
        }
        assert_eq!(seen, ["name", "height", "inner"]);
    }
}
