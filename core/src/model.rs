//! Data model: the structs that mirror our two tables, plus the crate error type.
//!
//! `kind` and `source` are stored as plain TEXT in SQL (the schema is
//! string-typed and AI features will invent new kinds). We keep them as
//! `String` on the structs rather than Rust enums so the model never lies about
//! what the DB can hold. Convenience constants below name the values we care
//! about today without closing the set.

use serde::{Deserialize, Serialize};

/// A row of `people`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub team: Option<String>,
    pub title: Option<String>,
    pub notes: Option<String>,
    pub created_at: String, // ISO8601; stored/compared as text, same as SQLite does
    pub updated_at: String,
}

/// A row of `relationships`. `reports_to` edges are just one `kind` among many.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relationship {
    pub id: i64,
    pub from_id: i64,
    pub to_id: i64,
    pub kind: String,
    pub notes: Option<String>,
    pub confidence: f64,
    pub source: String,
    pub created_at: String,
}

/// Well-known relationship kinds. Not exhaustive — the column is open TEXT.
pub mod kind {
    pub const REPORTS_TO: &str = "reports_to";
    pub const MENTORS: &str = "mentors";
    pub const COLLABORATES_WITH: &str = "collaborates_with";
}

/// Well-known edge sources. Manual entries use MANUAL; AI uses INFERRED.
pub mod source {
    pub const MANUAL: &str = "manual";
    pub const INFERRED: &str = "inferred";
    pub const IMPORTED: &str = "imported";
}

/// The crate's error type. `thiserror` is the standard pick for a *library*:
/// it gives each variant a real type and a Display message, and `#[from]`
/// auto-converts upstream errors so call sites can just use `?`.
#[derive(Debug, thiserror::Error)]
pub enum OrgError {
    /// Anything the libSQL driver returns (connect, prepare, query, exec).
    #[error("database error: {0}")]
    Db(#[from] libsql::Error),

    /// JSON (de)serialization during export/import.
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),

    /// A person id that doesn't exist.
    #[error("no person with id {0}")]
    PersonNotFound(i64),

    /// Caller asked for something the data forbids (e.g. self-reporting).
    #[error("invalid operation: {0}")]
    Invalid(String),
}

/// Crate-wide result alias so signatures read cleanly.
pub type Result<T> = std::result::Result<T, OrgError>;
