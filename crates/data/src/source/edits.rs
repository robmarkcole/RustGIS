//! Changes and outcomes shared by editable source tables.

use crate::Feature;

/// Edits to save to a table, by its key.
#[derive(Debug, Default)]
pub struct Edits {
    /// Rows changed: each as it is now, with the version it was read at.
    pub updates: Vec<(Feature, i64)>,
    /// Rows deleted: their keys and the versions they were read at.
    pub deletes: Vec<(u64, i64)>,
    /// New rows. The source assigns their keys according to its insertion policy.
    pub inserts: Vec<Feature>,
}

impl Edits {
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.deletes.is_empty() && self.inserts.is_empty()
    }
}

/// Why edits weren't saved: a row in the database isn't as it was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conflict {
    /// A row changed in the database since it was read.
    Changed(u64),
    /// A row deleted from the database since it was read.
    Deleted(u64),
    /// A new row's key is already taken.
    Exists(u64),
}

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Conflict::Changed(k) => write!(f, "row {} was changed in the database since it was read", k.cast_signed()),
            Conflict::Deleted(k) => write!(f, "row {} was deleted from the database since it was read", k.cast_signed()),
            Conflict::Exists(k) => write!(f, "a row with key {} is already in the database", k.cast_signed()),
        }
    }
}

/// What saving edits did.
#[derive(Debug, Default)]
pub struct Saved {
    /// Each new row's feature id and the key it was saved with.
    pub inserted: Vec<(u64, u64)>,
    pub updated: usize,
    /// Rows edited only in what the table doesn't hold, left as they are.
    pub unchanged: usize,
    pub deleted: usize,
    /// The version of each row inserted or updated, as saved.
    pub versions: Vec<(u64, i64)>,
    /// The layer's fields the table has no column for, which weren't saved.
    pub skipped_fields: Vec<String>,
    /// For each of the layer's fields, the date-times at an offset other than UTC's written to a
    /// column that holds instants in UTC: the moment is kept, the offset isn't.
    pub utc_dates: Vec<usize>,
    /// For each of the layer's fields, the date-times with an offset written to a column without
    /// a time zone: the clock time is kept, the offset isn't.
    pub zoneless_dates: Vec<usize>,
    /// For each of the layer's fields, the date-times without a zone written to a column that
    /// holds instants: taken as UTC.
    pub assumed_utc: Vec<usize>,
}
