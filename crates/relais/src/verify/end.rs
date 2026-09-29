//! How a recorded check ended — or that its receipt never said (#108).
//!
//! `CheckOutcome::ended` was added after the first receipts were
//! written, so some stored receipts have no such field. Reading them as
//! any [`Ended`] would invent a termination the receipt never claimed;
//! [`CheckEnd`] reads them as unrecorded instead.
//!
//! The unrecorded state lives in this file's private field and nowhere
//! else: the only way to build a `CheckEnd` outside deserialization is
//! [`CheckEnd::new`], which takes a real [`Ended`]. A live check
//! therefore always records how it ended, and the compiler says so:
//!
//! ```compile_fail
//! // The field is private, so no live code can name the unrecorded end.
//! let _ = relais::verify::CheckEnd(None);
//! ```

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::procs::Ended;

/// The end of a check's process as a receipt records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckEnd(Option<Ended>);

impl CheckEnd {
    /// A check that ran and ended this way.
    pub fn new(ended: Ended) -> Self {
        Self(Some(ended))
    }

    /// The end the receipt claimed; `None` for a receipt that predates
    /// the field.
    pub fn recorded(self) -> Option<Ended> {
        self.0
    }

    /// Only a recorded success passes: an unrecorded end says nothing
    /// about having succeeded.
    pub fn succeeded(self) -> bool {
        self.0.is_some_and(Ended::succeeded)
    }

    /// For a log line or a receipt.
    pub fn describe(self) -> String {
        match self.0 {
            Some(ended) => ended.describe(),
            None => "end not recorded".to_string(),
        }
    }
}

impl From<Ended> for CheckEnd {
    fn from(ended: Ended) -> Self {
        Self::new(ended)
    }
}

/// An unrecorded end equals no [`Ended`].
impl PartialEq<Ended> for CheckEnd {
    fn eq(&self, other: &Ended) -> bool {
        self.0 == Some(*other)
    }
}

impl Serialize for CheckEnd {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

/// A field absent from the document reaches here through serde's
/// missing-field path, which offers `deserialize_option` a `none` —
/// that is the only place the unrecorded end is made.
impl<'de> Deserialize<'de> for CheckEnd {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<Ended>::deserialize(deserializer).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_end_reads_as_unrecorded_and_never_passes() {
        #[derive(Deserialize)]
        struct Holder {
            ended: CheckEnd,
        }
        let holder: Holder = serde_json::from_str("{}").expect("a missing end parses");
        assert_eq!(holder.ended.recorded(), None);
        assert!(!holder.ended.succeeded());
        assert_ne!(holder.ended, Ended::Exited(0));
    }

    #[test]
    fn a_recorded_end_round_trips_as_the_end_it_was() {
        let end = CheckEnd::new(Ended::TimedOut);
        let json = serde_json::to_string(&end).expect("serializes");
        assert_eq!(json, "\"timed_out\"");
        assert_eq!(serde_json::from_str::<CheckEnd>(&json).unwrap(), end);
        assert!(CheckEnd::new(Ended::Exited(0)).succeeded());
    }
}
