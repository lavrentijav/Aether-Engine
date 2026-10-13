//! What reading a player's saved record can come back as.
//!
//! Collapsing "there is no record" and "there is a record we cannot read" into
//! one `None` is how a player loses everything: the unreadable inventory reads
//! as empty, the player plays on with nothing, and the first save writes that
//! nothing over the real one. So a read is three-valued, and only `Missing`
//! may ever be treated as a fresh start.

use std::fmt;

/// The outcome of reading one saved record.
#[derive(Debug, PartialEq)]
pub enum Stored<T> {
    /// A record was there and decoded.
    Found(T),
    /// No record: a player who never saved this, and the one case where a
    /// default is correct.
    Missing,
    /// A record exists (or may exist) but could not be read. Nothing may be
    /// written over it until someone has looked.
    Unreadable(Unreadable),
}

/// Why a record could not be read.
#[derive(Debug, PartialEq)]
pub enum Unreadable {
    /// The store itself failed. Possibly transient; the record is untouched.
    Storage(String),
    /// The bytes are there but do not decode: corrupt, or written by a newer
    /// format. Retrying will not help.
    Corrupt { len: usize },
}

impl fmt::Display for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unreadable::Storage(e) => write!(f, "storage error: {e}"),
            Unreadable::Corrupt { len } => write!(f, "{len}-byte record does not decode"),
        }
    }
}

/// Classify a raw read of a saved record.
pub fn classify<T, E: fmt::Display>(
    read: Result<Option<Vec<u8>>, E>,
    decode: impl FnOnce(&[u8]) -> Option<T>,
) -> Stored<T> {
    match read {
        Err(e) => Stored::Unreadable(Unreadable::Storage(e.to_string())),
        Ok(None) => Stored::Missing,
        Ok(Some(blob)) => match decode(&blob) {
            Some(v) => Stored::Found(v),
            None => Stored::Unreadable(Unreadable::Corrupt { len: blob.len() }),
        },
    }
}

impl<T> Stored<T> {
    /// Why the record could not be read, if it could not.
    pub fn unreadable(&self) -> Option<&Unreadable> {
        match self {
            Stored::Unreadable(why) => Some(why),
            _ => None,
        }
    }

    /// The record, if one was read; `None` for a player who never had one.
    /// Panics on `Unreadable` — callers must have refused those already.
    pub fn into_option(self) -> Option<T> {
        match self {
            Stored::Found(v) => Some(v),
            Stored::Missing => None,
            Stored::Unreadable(why) => {
                panic!("an unreadable saved record reached gameplay ({why}); it must be refused at login")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_one(b: &[u8]) -> Option<u8> {
        (b.len() == 1).then(|| b[0])
    }

    #[test]
    fn a_record_that_decodes_is_found() {
        let r: Result<_, String> = Ok(Some(vec![7]));
        assert_eq!(classify(r, decode_one), Stored::Found(7));
    }

    #[test]
    fn no_record_is_missing_and_only_that_is_a_fresh_start() {
        let r: Result<_, String> = Ok(None);
        assert_eq!(classify(r, decode_one), Stored::Missing);
    }

    #[test]
    fn a_record_that_does_not_decode_is_unreadable_not_missing() {
        let r: Result<_, String> = Ok(Some(vec![1, 2, 3]));
        assert_eq!(
            classify(r, decode_one),
            Stored::Unreadable(Unreadable::Corrupt { len: 3 })
        );
    }

    #[test]
    fn a_storage_error_is_unreadable_not_missing() {
        let r: Result<Option<Vec<u8>>, String> = Err("disk on fire".into());
        assert_eq!(
            classify(r, decode_one),
            Stored::Unreadable(Unreadable::Storage("disk on fire".into()))
        );
    }

    #[test]
    #[should_panic(expected = "must be refused at login")]
    fn unreadable_never_silently_becomes_none() {
        Stored::<u8>::Unreadable(Unreadable::Corrupt { len: 0 }).into_option();
    }
}
