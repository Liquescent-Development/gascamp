/// Errors from camp-core. Library code never panics (workspace lints deny
/// unwrap/expect/panic); every failure surfaces here.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ledger corrupt: {0}")]
    Corrupt(String),
    #[error("ledger schema version {found} unsupported (this build supports {supported})")]
    UnsupportedSchema { found: i64, supported: i64 },
    #[error("invalid event data for {event_type}: {reason}")]
    InvalidEventData { event_type: String, reason: String },
    #[error("bead {bead}: invalid transition: {reason}")]
    InvalidTransition { bead: String, reason: String },
    #[error("unknown bead {0}")]
    UnknownBead(String),
    #[error("unknown session {0}")]
    UnknownSession(String),
    #[error("unknown event type {0:?}")]
    UnknownEventType(String),
    #[error("config: {0}")]
    Config(String),
    #[error("unknown rig {0:?}")]
    UnknownRig(String),
    #[error("invalid rig prefix {0:?}: must match ^[a-z][a-z0-9]*$")]
    InvalidPrefix(String),
    #[error("invalid search query {query:?}: {reason}")]
    InvalidSearchQuery { query: String, reason: String },
    /// A formula cook failed before or around the ledger transaction
    /// (bad cook input, run-dir filesystem trouble). NOT a ledger
    /// integrity finding — that is `Corrupt`.
    #[error("cook: {0}")]
    Cook(String),
}
