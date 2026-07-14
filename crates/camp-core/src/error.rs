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
    #[error(
        "ledger schema version {found} unsupported (this build supports {supported}); \
         no auto-upgrade — re-init the camp (`camp backup`/`camp export` preserve history)"
    )]
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
    #[error("pack: {0}")]
    Pack(String),
    #[error(
        "unknown agent {name:?}; searched {searched:?} (packs in camp.toml order, then <camp>/agents/)"
    )]
    UnknownAgent { name: String, searched: Vec<String> },
    /// A formula cook failed before or around the ledger transaction
    /// (bad cook input, run-dir filesystem trouble). NOT a ledger
    /// integrity finding — that is `Corrupt`.
    #[error("cook: {0}")]
    Cook(String),
    /// A formula could not be COMPILED for a reason outside the key table:
    /// an unresolvable `extends` parent, an asset that no layer ships, a
    /// `description_file` escaping its pack root, an expansion cycle. A
    /// per-key verdict is a `Violation` or a `Refusal`; this is everything
    /// that is neither.
    #[error("formula: {0}")]
    Formula(String),
    /// An order is misconfigured or failed at the order level; the reason
    /// names the offending field where one exists (spec §9: parse errors
    /// name the order and the field).
    #[error("order {order:?}: {reason}")]
    Order { order: String, reason: String },
    /// A pack import failed (component §10 error table): a bad source, a
    /// missing pack.toml, a repo-escaping transitive source, a binding
    /// clash. The binding names the import; the reason is actionable.
    #[error("import {binding:?}: {reason}")]
    Import { binding: String, reason: String },
    /// A `camp export` failure that is not an order-translation finding:
    /// bad output directory, unreadable inputs, malformed run dirs.
    #[error("export: {0}")]
    Export(String),
    /// A `camp backup` failure: the destination already exists, the VACUUM
    /// INTO copy failed, or the copy did not pass `PRAGMA integrity_check`.
    #[error("backup: {0}")]
    Backup(String),
    /// Orders that cannot be expressed as gc order TOML (spec §15.3, plan
    /// decision 8). Listed in full; the flag named here is the contract's
    /// explicit opt-out.
    #[error(
        "export: {count} order(s) cannot be translated to gc order TOML:\n{details}\npass --skip-untranslatable to export without them"
    )]
    UntranslatableOrders { count: usize, details: String },
}
