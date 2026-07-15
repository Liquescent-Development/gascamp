//! cp-1 (control-plane spec §2, §2.1, §9): THE control wire.
//!
//! This module is the ONLY place in camp that constructs or parses a
//! `claude` control message. The format is undocumented, so it is pinned by
//! fixtures whose provenance is LABELLED (see
//! `tests/fixtures/control/PROVENANCE.md`): what was recorded from the real
//! CLI, what was derived from its shipped bundle, and what camp authored.
//! Concentrating the wire here is what makes those pins meaningful — a
//! second construction site elsewhere would not be covered by them.
//!
//! Two directions:
//!
//! - **Outbound** (`ParentMessage`) — what campd writes into a worker's
//!   held stdin: an `interrupt` control request, and the deterministic
//!   refusal of a `request_user_dialog` (§9).
//! - **Inbound** (`parse_worker_line`) — what campd reads back out of the
//!   worker's stdout file (cp-0's read channel tails it): a
//!   `control_response`, a `can_use_tool` request, a `request_user_dialog`
//!   request, or — the overwhelmingly common case — an ordinary stream line
//!   that camp passes through and never interprets.
//!
//! **D3, the surface rule.** The control surface is STRICT and the stream
//! surface is TRANSPARENT. Strictness keys on `type.starts_with("control")`,
//! so a future `control_notify` camp does not know becomes a LOUD fault
//! rather than content forwarded to a subscriber as if it were the worker's
//! own words. Everything that is not `control*` passes through verbatim.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Context as _;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use mio::Token;

use super::dispatch::{ControlWrite, Dispatcher, NudgeOutcome};
use super::event_loop::Conn;
use super::patrol::PatrolRuntime;
use super::read_channel::{Disposed, ReadChannelRuntime, StreamLine};
use super::socket::{Response, SessionInfo};

/// Every request id camp mints carries this prefix. It is what lets campd
/// tell its OWN request apart from one the CLI minted for itself (a
/// `can_use_tool` id, say) in a single glance at a ledger.
pub const REQUEST_ID_PREFIX: &str = "camp-";

/// A fresh control request id: `camp-<uuid-v4>`. Uniqueness is what makes
/// the pending table, the ledger correlation and a retrying caller's
/// de-duplication all work; a counter would collide across a restart.
pub fn new_request_id() -> String {
    format!("{REQUEST_ID_PREFIX}{}", uuid::Uuid::new_v4())
}

// ---------------------------------------------------------------------------
// OUTBOUND — what campd writes into a worker's held stdin.
//
// These are `#[derive(Serialize)]` STRUCTS, not `serde_json::json!` calls,
// and that is load-bearing: a struct serializes in DECLARATION order, while
// `json!` builds a `Map` that serde_json (no `preserve_order`) emits
// ALPHABETICALLY. The fixtures pin the bytes, so the two must not disagree.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct InterruptEnvelope<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    request_id: &'a str,
    request: InterruptBody,
}

#[derive(Serialize)]
struct InterruptBody {
    subtype: &'static str,
}

#[derive(Serialize)]
struct ErrorResponseEnvelope<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    response: ErrorResponseBody<'a>,
}

#[derive(Serialize)]
struct ErrorResponseBody<'a> {
    subtype: &'static str,
    request_id: &'a str,
    error: &'a str,
}

/// A message campd sends INTO a worker's stdin.
pub enum ParentMessage {
    /// §4.1 `session.interrupt`. The CLI acks it with a `control_response`
    /// on its stdout, which cp-0's read channel tails back to campd.
    Interrupt { request_id: String },
    /// §9: camp is not a human and has no dialog to show. Every
    /// `request_user_dialog` gets this DETERMINISTIC refusal — because the
    /// alternative is a worker blocked forever waiting for an answer that
    /// can never come, holding a dispatch slot.
    DialogRefusal { request_id: String },
}

impl ParentMessage {
    /// The exact newline-terminated line to write into the pipe.
    ///
    /// `Result`, not a panic: this is library code and clippy denies
    /// `unwrap`/`expect`/`panic` here (invariant 5).
    pub fn to_line(&self) -> anyhow::Result<String> {
        let mut line = match self {
            ParentMessage::Interrupt { request_id } => serde_json::to_string(&InterruptEnvelope {
                kind: "control_request",
                request_id,
                request: InterruptBody {
                    subtype: "interrupt",
                },
            })?,
            ParentMessage::DialogRefusal { request_id } => {
                serde_json::to_string(&ErrorResponseEnvelope {
                    kind: "control_response",
                    response: ErrorResponseBody {
                        subtype: "error",
                        request_id,
                        error: "camp does not support interactive dialogs",
                    },
                })?
            }
        };
        line.push('\n');
        Ok(line)
    }
}

// ---------------------------------------------------------------------------
// INBOUND — what campd reads out of a worker's stdout file.
// ---------------------------------------------------------------------------

/// A line camp could not make sense of, carrying the line itself so the
/// fault event can name it (§2.1: loud, never swallowed).
#[derive(Debug)]
pub struct ControlWireError {
    pub line: String,
    pub reason: String,
}

/// One parsed line from a worker's stdout.
#[derive(Debug)]
pub enum WorkerMessage<'a> {
    /// The worker answered one of camp's control requests.
    ControlResponse {
        request_id: String,
        ok: bool,
        /// The `error` string on a failure; the inner `response` object,
        /// serialized, on a success. Diagnostic — camp never routes on it.
        detail: String,
    },
    /// §5.3.1: the CLI is asking permission to run a tool. Structurally
    /// UNREACHABLE in cp-1 (camp does not pass `--permission-prompt-tool`),
    /// so its arrival is a loud fault, and phase 3 owns the answer.
    CanUseTool {
        request_id: String,
        tool_name: String,
    },
    /// §9: the CLI wants to show a human a dialog. camp refuses, every time.
    RequestUserDialog { request_id: String },
    /// D3: everything that is not `control*`. camp passes it through and
    /// never interprets it.
    Stream(
        #[allow(dead_code)]
        // PERMANENT: never read in production. Subscribers are fed from the
        // FILE by pump (D6"), not from this variant — it exists so
        // parse_worker_line is TOTAL (D3's transparent surface) and so the
        // passthrough test can assert the bytes are unchanged.
        &'a str,
    ),
}

/// The permissive envelope. **Deliberately NOT `deny_unknown_fields`** (C9):
/// the peer is a minified bundle whose full key set cannot be proven by any
/// grep, and a parse that breaks when the CLI adds a key is a parse that
/// breaks in production. camp reads the keys it needs and ignores the rest.
#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    request_id: Option<String>,
    request: Option<serde_json::Value>,
    response: Option<serde_json::Value>,
}

fn wire_err(line: &str, reason: impl Into<String>) -> ControlWireError {
    ControlWireError {
        line: line.to_owned(),
        reason: reason.into(),
    }
}

/// Parse one complete stdout line. TOTAL: every line is either a control
/// message camp understands, an ordinary stream line, or a LOUD error.
///
/// D3's prefix rule lives here: strictness keys on `type.starts_with(
/// "control")`, never on an exhaustive list of known control types. A
/// `control_notify` the CLI adds tomorrow therefore FAULTS instead of being
/// forwarded to a subscriber as if it were the worker's own output.
pub fn parse_worker_line(line: &str) -> Result<WorkerMessage<'_>, ControlWireError> {
    let envelope: Envelope = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => {
            // A non-JSON line reaching HERE is a fault. (cp-0's `drain_one`
            // only hands over lines it already parsed, so in production this
            // arm is reached by a line that is valid JSON but not an object
            // with a `type` — belt and braces.)
            return Err(wire_err(line, format!("not a control envelope: {e}")));
        }
    };

    // D3: the transparent stream surface. The overwhelming majority of lines
    // land here, and camp never looks inside them.
    if !envelope.kind.starts_with("control") {
        return Ok(WorkerMessage::Stream(line));
    }

    match envelope.kind.as_str() {
        "control_response" => {
            let body = envelope
                .response
                .ok_or_else(|| wire_err(line, "a control_response with no `response` object"))?;
            // VERIFIED NESTING: the id is INSIDE `response`, not at the top
            // level (the bundle: `response:{subtype:"error",request_id:…}`).
            let request_id = body["request_id"]
                .as_str()
                .ok_or_else(|| wire_err(line, "a control_response with no `response.request_id`"))?
                .to_owned();
            match body["subtype"].as_str() {
                Some("success") => Ok(WorkerMessage::ControlResponse {
                    request_id,
                    ok: true,
                    detail: body["response"].to_string(),
                }),
                Some("error") => Ok(WorkerMessage::ControlResponse {
                    request_id,
                    ok: false,
                    // The `error` KEY is the verified one. The fallback is
                    // reachable only if the CLI stops sending it — in which
                    // case the fixture test is already RED and telling us so.
                    detail: body["error"]
                        .as_str()
                        .unwrap_or("the CLI reported an error but named no reason")
                        .to_owned(),
                }),
                other => Err(wire_err(
                    line,
                    format!("a control_response with an unknown subtype {other:?}"),
                )),
            }
        }
        "control_request" => {
            let body = envelope
                .request
                .ok_or_else(|| wire_err(line, "a control_request with no `request` object"))?;
            // The id is at the TOP level for a request (verified in the
            // bundle: `type==="control_request"&&"request_id" in e`).
            let request_id = envelope
                .request_id
                .ok_or_else(|| wire_err(line, "a control_request with no `request_id`"))?;
            match body["subtype"].as_str() {
                Some("can_use_tool") => Ok(WorkerMessage::CanUseTool {
                    request_id,
                    tool_name: body["tool_name"].as_str().unwrap_or_default().to_owned(),
                }),
                // `dialog_kind`'s VALUE SET was not recoverable from the
                // bundle (it is a minified variable), so camp NEVER keys on
                // it. It reads the id and refuses. That is a choice that
                // cannot rot.
                Some("request_user_dialog") => Ok(WorkerMessage::RequestUserDialog { request_id }),
                other => Err(wire_err(
                    line,
                    format!("a control_request with an unknown subtype {other:?}"),
                )),
            }
        }
        // D3's PREFIX RULE. Not an oversight — the whole point.
        other => Err(wire_err(
            line,
            format!(
                "unknown control message type {other:?}. camp refuses to guess at a control \
                 message it does not know: forwarding it to a subscriber would present the \
                 CLI's protocol chatter as the worker's own output (§2.1)"
            ),
        )),
    }
}

// ---------------------------------------------------------------------------
// cp-1 (§2.1): the pending-request table.
// ---------------------------------------------------------------------------

/// How long a session may be SILENT with a control request outstanding before
/// campd declares the protocol broken (§2.1). A BOUND on one operation, not a
/// wakeup: it joins `min_deadline` only while something is pending
/// (invariant 1).
///
/// D7/C11 — THIS MEASURES SILENCE, NOT ELAPSED TIME. `note_activity` resets it
/// on ANY stream line from the session. That matters because of an UNVERIFIED
/// property of the CLI: it is not known whether it reads control messages from
/// stdin WHILE A TURN IS STREAMING (every interrupt exercised anywhere in this
/// repo, fake or real, is PRE-turn). If the CLI queues control messages until
/// the turn completes, an elapsed-time deadline would fire a FALSE
/// `control.failed` on any turn longer than 30s. A SILENCE deadline does not: a
/// worker producing output is alive, and `control.failed` now means "the session
/// went quiet for 30s with an unanswered request" — a real fault under EITHER
/// semantics. The residual (a worker that goes silent mid-turn with its
/// interrupt queued) is REPAIRED, not hidden: a late answer appends a
/// correction (C11).
///
/// G6/A3 — AND IT IS NOT ENOUGH ON ITS OWN. A worker that NEVER goes quiet (a
/// long tool loop; anything under cp-4's `--include-partial-messages`) would
/// have its deadline pushed forward FOREVER, so an interrupt the CLI never
/// processes would fault NEVER — §2.1's swallowed timeout, through the front
/// door. And there is no backstop: patrol's stall ladder is ALSO activity-driven
/// (`drain_touched` resets its timer on transcript activity), so a chatty worker
/// is never stalled EITHER. Both safety nets are the same net, with a hole in
/// exactly this shape. Hence the ABSOLUTE CEILING below, which nothing resets.
pub const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// G6: the absolute ceiling on ONE control request, measured from `created_at`
/// and RESET BY NOTHING. A worker that has been producing output for five
/// minutes without acknowledging an interrupt is broken, under either mid-turn
/// semantics.
///
/// The trade, stated: an elapsed-time bound can fire a FALSE fault on a
/// legitimately long queued interrupt — but C11 makes that fault SELF-REPAIRING
/// (a late answer appends `control.responded{late:true}` naming the fault it
/// corrects, and rehydration preserves that across a restart). D7 alone traded a
/// CORRECTABLE FALSE POSITIVE for an UNCORRECTABLE FALSE NEGATIVE, which is
/// strictly worse under invariant 3.
pub const CONTROL_RESPONSE_CEILING: Duration = Duration::from_secs(300);

/// The cap on outstanding control requests. Past it `serve_interrupt` refuses
/// LOUDLY — so neither an overseer loop nor a hostile local client can grow the
/// pending table, or the ledger, without bound.
pub const MAX_PENDING_CONTROL_REQUESTS: usize = 64;

/// One outstanding control request.
struct Pending {
    session: String,
    verb: &'static str,
    /// G7: captured at `serve_interrupt`, so EVERY fault this request may later
    /// produce carries the SAME provenance as the `session.interrupted` it
    /// answers. (Rev 3 built its fault EventInputs with rig/bead = None, so a
    /// fault and its cause disagreed about which bead they belonged to.)
    rig: Option<String>,
    bead: Option<String>,
    /// G6: NEVER moves. The ceiling is computed from this.
    created_at: Timestamp,
    /// D7: the SILENCE deadline. `note_activity` pushes it forward.
    deadline: Timestamp,
}

impl Pending {
    /// THE SINGLE EXPIRY PREDICATE. Both `poll_timeout` and `expire_pending`
    /// use it, so they can never disagree about when a request is due.
    fn due_at(&self) -> Timestamp {
        let ceiling = self.created_at + signed(CONTROL_RESPONSE_CEILING);
        self.deadline.min(ceiling)
    }
}

/// jiff's `SignedDuration` from a `std::time::Duration`. Both constants are
/// small and constant, so the fallback is unreachable — but clippy denies
/// `unwrap` in library code, and a silent fallback is better than a panic in
/// campd's hot loop.
fn signed(d: Duration) -> SignedDuration {
    SignedDuration::try_from(d).unwrap_or(SignedDuration::from_secs(30))
}

pub struct ControlRuntime {
    pending: HashMap<String, Pending>,
    /// ANSWERED, or settled TERMINALLY (a cause from which no answer can ever
    /// arrive — see `TERMINAL_CAUSES`). A re-read `control_response` for one of
    /// these is a TRUE duplicate => `None` (B6).
    ///
    /// request_id -> session. It carries the SESSION, not just the id, because
    /// `forget_session` (G7) must be able to PRUNE it — and a bare id set cannot
    /// be pruned by session. That pruning is half of what bounds this map by
    /// LIVE sessions rather than by the ledger's whole history.
    answered: HashMap<String, String>,
    /// C11/G5: TIMED OUT (cause `silence_timeout` or `ceiling_timeout`) — campd
    /// has already appended `control.failed` saying the worker never answered. A
    /// `control_response` for one of these is NOT a duplicate: it is NEW
    /// INFORMATION saying that fault was PREMATURE, and it appends a CORRECTION.
    ///
    /// Rev 3's `rehydrate` collapsed these into `answered`, which silently
    /// swallowed a late answer ACROSS A RESTART — the exact bug C11 exists to
    /// forbid. That was only possible because `control.failed` had no
    /// machine-readable cause; G5 added one, and `rehydrate` routes on it.
    timed_out: HashMap<String, Pending>,
    /// §4.4: the per-subscriber outbound buffer cap. A STOP, not a kill.
    subscriber_buffer_bytes: usize,
    /// R1: how long a peer may accept ZERO bytes before campd drops it.
    stall_timeout: Duration,
    /// The subscriber registry, keyed by connection token.
    subscribers: HashMap<Token, Subscriber>,
    /// `pump` cannot take `&mut Ledger` (it is called with a `&mut Conn` already
    /// borrowed out of the same map), so its durable events ride this collector —
    /// cp-0's `cap_breaches`/`parse_errors` mold — drained by the caller.
    pending_events: Vec<EventInput>,
    /// G11: the `over_cap` `patrol.degraded` dedupe. N subscribers hit the SAME
    /// over-cap line and must not append N identical events.
    ///
    /// **RESIDUAL, RECORDED: it is never pruned** — not by `forget`, not by
    /// `forget_session`, not by disposal. It holds one `(session, offset)` entry per
    /// distinct over-cap LINE for campd's whole life. Over-cap lines are rare (a line
    /// bigger than 1 MiB), so this is bounded in practice rather than by
    /// construction; a phase that makes them common must prune it with the session.
    degraded_seen: HashSet<(String, u64)>,
    next_subscription: u64,
    /// The most recently computed fleet model, refreshed by `fanout` and by
    /// `serve_fleet_subscribe`. It is what the WRITABLE-edge `pump` diffs against
    /// when it continues a cap-STOPped fleet delta (that path has no ledger in
    /// scope). Empty when no fleet subscriber exists — computing it then would be
    /// pure waste (invariant 1).
    fleet_model: Vec<SessionInfo>,
}

impl ControlRuntime {
    /// Test-only: production goes through `with_stall_timeout`, which reads the
    /// env override (the `max_stream_bytes_from_env` mold).
    #[cfg(test)]
    pub fn new(subscriber_buffer_bytes: usize) -> ControlRuntime {
        ControlRuntime::with_stall_timeout(
            subscriber_buffer_bytes,
            SUBSCRIBER_STALL_TIMEOUT_DEFAULT,
        )
    }

    pub fn with_stall_timeout(
        subscriber_buffer_bytes: usize,
        stall_timeout: Duration,
    ) -> ControlRuntime {
        ControlRuntime {
            pending: HashMap::new(),
            answered: HashMap::new(),
            timed_out: HashMap::new(),
            subscriber_buffer_bytes,
            stall_timeout,
            subscribers: HashMap::new(),
            pending_events: Vec::new(),
            degraded_seen: HashSet::new(),
            next_subscription: 0,
            fleet_model: Vec::new(),
        }
    }

    pub fn track_pending(
        &mut self,
        request_id: String,
        session: String,
        verb: &'static str,
        rig: Option<String>,
        bead: Option<String>,
        now: Timestamp,
    ) {
        self.pending.insert(
            request_id,
            Pending {
                session,
                verb,
                rig,
                bead,
                created_at: now,
                deadline: now + signed(CONTROL_RESPONSE_TIMEOUT),
            },
        );
    }

    /// D7/C11: ANY stream line from a session resets the SILENCE deadline of
    /// every request outstanding against it. It NEVER touches `created_at` — the
    /// G6 ceiling is reset by nothing, which is the whole point of having it.
    pub fn note_activity(&mut self, session: &str, now: Timestamp) {
        for p in self.pending.values_mut() {
            if p.session == session {
                p.deadline = now + signed(CONTROL_RESPONSE_TIMEOUT);
            }
        }
    }

    /// THE control plane's whole wakeup story. `None` when nothing is pending —
    /// an idle campd with idle subscribers still blocks FOREVER (invariant 1).
    ///
    /// Three sources, and each corresponds to a state with NO other wakeup:
    ///
    /// 1. a pending control request's silence/ceiling deadline;
    /// 2. a subscriber with PUMPABLE FILE WORK and an EMPTY `out` — no fd will
    ///    ever signal that, so it needs an armed continuation;
    /// 3. R1: a peer that accepts NOTHING generates NO events at all — not a
    ///    WRITABLE edge, not an EOF — so the stall drop needs its own deadline,
    ///    and it is the ONLY thing that can ever fire for that subscriber.
    ///
    /// G2 — A NON-EMPTY `out` MUST NOT ARM ANYTHING. It means the last write
    /// returned WouldBlock, and the correct wakeup for that is the WRITABLE EDGE,
    /// which is already registered. Arming ZERO on top of it turns every blocked
    /// write into a SPIN — and since macOS's socket send buffer (~8 KiB) is far
    /// smaller than one chunk's worth of frames, EVERY healthy subscriber
    /// WouldBlocks on essentially every chunk. campd would spin for the duration of
    /// any stream (invariant 1, §4.3).
    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration> {
        let earliest_control = self.pending.values().map(Pending::due_at).min();

        // B3(c): `|| s.held` — a line HELD at `scan == tail` (the normal terminal
        // state of a catch-up that ran at the cap) is real, pending work. Requiring
        // `scan < tail` strands it: `blocked_since` is None (the peer IS reading),
        // no WRITABLE edge is pending once `out` drains, and the last line of the
        // history is never delivered.
        let subscriber_work = self.subscribers.values().any(|s| match &s.source {
            Source::File(fs) => s.out.is_empty() && (fs.held || fs.scan < fs.tail) && !fs.end_sent,
            // A fleet fill fully drains per pump loop or WouldBlocks — no
            // empty-`out`-with-pending state persists, so no zero-arm is needed.
            Source::Fleet(_) => false,
        });

        let earliest_stall = self
            .subscribers
            .values()
            .filter_map(|s| s.out.blocked_since)
            .map(|t| t + signed(self.stall_timeout))
            .min();

        let mut best: Option<Duration> = None;
        for candidate in [
            earliest_control.map(|d| duration_until(d, now)),
            if subscriber_work {
                Some(Duration::ZERO)
            } else {
                None
            },
            earliest_stall.map(|d| duration_until(d, now)),
        ]
        .into_iter()
        .flatten()
        {
            best = Some(best.map_or(candidate, |b: Duration| b.min(candidate)));
        }
        best
    }

    /// Every request past its `due_at` becomes a durable `control.failed` and
    /// MOVES to `timed_out` (never `answered` — a late answer must still be able
    /// to correct it, C11).
    pub fn expire_pending(&mut self, now: Timestamp) -> Vec<EventInput> {
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, p)| p.due_at() <= now)
            .map(|(id, _)| id.clone())
            .collect();

        let mut events = Vec::new();
        for id in due {
            let Some(p) = self.pending.remove(&id) else {
                continue;
            };
            // The cause is derived by comparing THE TWO BOUNDS — never either
            // against `now`. A wake delayed past BOTH bounds would otherwise
            // report `silence_timeout` when the CEILING is what actually
            // expired: an invariant-3 false cause.
            let ceiling = p.created_at + signed(CONTROL_RESPONSE_CEILING);
            let (cause, reason) = if p.deadline <= ceiling {
                (
                    "silence_timeout",
                    format!(
                        "the session {} went quiet for {}s with {} (request_id {id}) unanswered \
                         — §2.1: a control response that never arrives is a fault, never a \
                         swallowed timeout",
                        p.session,
                        CONTROL_RESPONSE_TIMEOUT.as_secs(),
                        p.verb,
                    ),
                )
            } else {
                (
                    "ceiling_timeout",
                    format!(
                        "the session {} produced output for {}m but never answered request_id \
                         {id} ({}). A worker that keeps talking pushes its silence deadline \
                         forward forever, so this ABSOLUTE ceiling is what stops the timeout \
                         being swallowed (§2.1)",
                        p.session,
                        CONTROL_RESPONSE_CEILING.as_secs() / 60,
                        p.verb,
                    ),
                )
            };
            events.push(EventInput {
                kind: EventType::ControlFailed,
                rig: p.rig.clone(),
                actor: "campd".into(),
                bead: p.bead.clone(),
                data: serde_json::json!({
                    "session": p.session,
                    "request_id": id,
                    "verb": p.verb,
                    "cause": cause,
                    "reason": reason,
                }),
            });
            self.timed_out.insert(id, p);
        }
        events
    }

    /// A worker answered. Four cases, and every one of them is a decision:
    ///
    /// - PENDING => the normal path. `control.responded{late:false}`.
    /// - TIMED_OUT => C11's CORRECTION. `control.responded{late:true}`, naming
    ///   the premature fault. NOT a duplicate — it is new information.
    /// - ANSWERED => a TRUE duplicate (a restart re-read the same line). None.
    /// - unknown => §2.1: a response for an id camp never sent is a FAULT.
    pub fn resolve(&mut self, request_id: &str, ok: bool, detail: String) -> Option<EventInput> {
        if let Some(p) = self.pending.remove(request_id) {
            self.answered
                .insert(request_id.to_owned(), p.session.clone());
            return Some(EventInput {
                kind: EventType::ControlResponded,
                rig: p.rig,
                actor: "campd".into(),
                bead: p.bead,
                data: serde_json::json!({
                    "session": p.session,
                    "request_id": request_id,
                    "verb": p.verb,
                    "ok": ok,
                    "detail": detail,
                    "late": false,
                }),
            });
        }

        if let Some(p) = self.timed_out.remove(request_id) {
            self.answered
                .insert(request_id.to_owned(), p.session.clone());
            return Some(EventInput {
                kind: EventType::ControlResponded,
                rig: p.rig,
                actor: "campd".into(),
                bead: p.bead,
                data: serde_json::json!({
                    "session": p.session,
                    "request_id": request_id,
                    "verb": p.verb,
                    "ok": ok,
                    "detail": format!(
                        "{detail} — this answer arrived AFTER control.failed declared the \
                         request unanswered. That fault was PREMATURE; this event is the \
                         correction (§2.1, invariant 3)"
                    ),
                    "late": true,
                }),
            });
        }

        if self.answered.contains_key(request_id) {
            // B6: a true duplicate. campd re-read a line it had already
            // ingested (a restart re-tails from the persisted offset). Appending
            // a second `control.responded` would be a lie about what happened.
            return None;
        }

        Some(EventInput {
            kind: EventType::ControlFailed,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({
                "request_id": request_id,
                "cause": "unknown_request",
                "reason": format!(
                    "a control_response arrived for request_id {request_id}, which camp never \
                     sent. Either the worker invented it or camp's pending table lost it — \
                     both are protocol faults, and §2.1 says they are loud"
                ),
            }),
        })
    }

    /// B6/G5: rebuild the pending table from the ledger after a restart.
    ///
    /// Scan `session.interrupted` for the ids camp sent, then ROUTE EACH ONE on
    /// the `cause` DISCRIMINANT of its `control.failed`, if any:
    ///
    /// - answered in `control.responded`            => `answered`
    /// - `control.failed{silence|ceiling_timeout}`  => `timed_out` (a late
    ///   answer must STILL correct it — rev 3 put these in `answered` and
    ///   SWALLOWED the answer)
    /// - `control.failed{any TERMINAL cause}`       => `answered`
    /// - in neither                                 => still pending, with a
    ///   FRESH deadline (the previous life's clock is not ours, and a worker
    ///   waiting across a restart deserves the full window)
    ///
    /// THE LIVENESS FILTER, and it is required. `forget_session` prunes
    /// `timed_out` in MEMORY, but the LEDGER holds every
    /// `session.interrupted` + `control.failed` pair FOREVER — so a rehydrate
    /// with no liveness filter would reconstruct a `timed_out` row for every
    /// interrupt that ever timed out in the ledger's whole history, on every
    /// campd start. The "bounded by live sessions" claim holds WITHIN a campd
    /// life and is FALSE across a restart without this. A request whose session
    /// is `stopped`/`crashed` is skipped: that session is gone, nothing can
    /// re-read its stream, and no correction can ever arrive.
    ///
    /// Bounded: three full-type scans of the ledger, once per campd life.
    pub fn rehydrate(&mut self, ledger: &Ledger, now: Timestamp) -> anyhow::Result<Rehydrated> {
        let events = ledger.events_range(1, None)?;

        let mut sent: Vec<(String, Pending)> = Vec::new();
        let mut responded: HashSet<String> = HashSet::new();
        let mut failed: HashMap<String, String> = HashMap::new();

        for event in &events {
            let id = event.data["request_id"].as_str().unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            match event.kind {
                EventType::SessionInterrupted => {
                    let Some(session) = event.data["session"].as_str() else {
                        continue;
                    };
                    sent.push((
                        id.to_owned(),
                        Pending {
                            session: session.to_owned(),
                            verb: "session.interrupt",
                            rig: event.rig.clone(),
                            bead: event.bead.clone(),
                            created_at: now,
                            deadline: now + signed(CONTROL_RESPONSE_TIMEOUT),
                        },
                    ));
                }
                EventType::ControlResponded => {
                    responded.insert(id.to_owned());
                }
                EventType::ControlFailed => {
                    let Some(cause) = event.data["cause"].as_str() else {
                        continue;
                    };
                    failed.insert(id.to_owned(), cause.to_owned());
                }
                _ => {}
            }
        }

        let mut live: HashMap<String, bool> = HashMap::new();
        let mut restored = 0usize;
        let mut events: Vec<EventInput> = Vec::new();

        for (id, p) in sent {
            let alive = match live.get(&p.session) {
                Some(v) => *v,
                None => {
                    let status = ledger.session_status(&p.session)?;
                    let v = status.as_deref() == Some("live");
                    live.insert(p.session.clone(), v);
                    v
                }
            };

            // ---- ALREADY SETTLED ------------------------------------------------
            // These branches run for LIVE and DEAD sessions alike, but a DEAD
            // session's rows are dropped rather than rebuilt — THAT is the liveness
            // filter, and that is ALL it may ever do. Nothing can arrive for a
            // session that is gone, so rebuilding its `answered`/`timed_out` rows
            // would grow both maps from the ledger's whole HISTORY on every start.
            if responded.contains(&id) {
                if alive {
                    self.answered.insert(id, p.session);
                }
                continue;
            }
            match failed.get(&id).map(String::as_str) {
                // The two CORRECTABLE causes: campd said "no answer came", and an
                // answer may yet come — but only from a session that still exists.
                Some("silence_timeout") | Some("ceiling_timeout") => {
                    if alive {
                        self.timed_out.insert(id, p);
                    }
                }
                // TERMINAL: nothing can ever arrive for these, so a stray
                // `control_response` is a duplicate, never a correction. The
                // partition lives in ONE place (`vocab::ControlFailureCause`), and
                // its `match` is exhaustive — a cause added by a later phase must
                // decide, at COMPILE TIME, which side it is on.
                Some(cause)
                    if camp_core::vocab::ControlFailureCause::parse(cause)
                        .is_some_and(|c| c.is_terminal()) =>
                {
                    if alive {
                        self.answered.insert(id, p.session);
                    }
                }
                // An unrecognized cause is a HARD ERROR, never a default: a value
                // this camp does not know means the ledger was written by a NEWER
                // camp, and guessing its meaning is exactly the silent divergence
                // invariant 5 forbids.
                //
                // ⚠ BD-1 WIDENED THIS `bail!`'s BLAST RADIUS, DELIBERATELY. Before
                // BD-1 a DEAD session's requests were skipped BEFORE this match ran,
                // so an unknown cause on a long-dead session was never even looked at.
                // They now fall THROUGH the match (that is the whole fix — a
                // never-answered request on a dead session must still get a terminal
                // event), so an unknown cause on a session that died months ago now
                // PREVENTS CAMPD FROM STARTING.
                //
                // That is the right trade and it is invariant-5-consistent: the only
                // way to reach it is a ledger written by a NEWER camp, the remedy is
                // named in the message ("Upgrade camp"), and the alternative —
                // silently ignoring a cause we cannot route — is precisely the
                // swallowed fault BD-1 exists to close. But it IS a widening, and it
                // is written down rather than discovered by an operator.
                Some(unknown) => {
                    anyhow::bail!(
                        "control.failed for request_id {id} carries cause {unknown:?}, which \
                         this camp does not know. The ledger was written by a newer camp; \
                         guessing what that cause means would silently change how a late \
                         control_response is handled. Upgrade camp."
                    );
                }

                // ---- NOT SETTLED: NO terminal event exists for this request ------
                //
                // BD-1. The liveness filter must NEVER reach here. A request that
                // reached no terminal state is the ONE case §2.1 forbids dropping,
                // and skipping it produced NO EVENT AT ALL: the ledger kept
                // `session.interrupted{request_id}` with no terminal event, FOREVER.
                //
                // A DEAD session cannot be disposed (it was never registered), so
                // `forget_session` NEVER RUNS for it. `rehydrate` is the only thing
                // left that can speak for the request — so it speaks, with the SAME
                // event `forget_session` would have produced.
                None if !alive => {
                    events.push(ControlRuntime::session_ended_fault(&id, &p));
                    // TERMINAL, and therefore IDEMPOTENT: the very
                    // `control.failed{session_ended}` we just appended routes this id
                    // to `answered` on the NEXT start, so a second restart appends
                    // nothing. (That is the new case this fix creates, and
                    // `a_restart_after_the_worker_also_died…` pins it by restarting
                    // twice.)
                }
                None => {
                    // Still live, still unanswered: a worker waiting across a restart
                    // deserves the full window, so the deadline is FRESH.
                    self.pending.insert(id, p);
                    restored += 1;
                }
            }
        }
        Ok(Rehydrated { restored, events })
    }

    /// §2.1: the ONE event that says "this session ended with the request
    /// unanswered". `forget_session` (campd is up, the session was disposed) and
    /// `rehydrate` (campd was DOWN when the worker died, so disposal never ran)
    /// must produce it IDENTICALLY — they are the same fact observed from two
    /// different places, and an operator must not be able to tell which path
    /// noticed.
    fn session_ended_fault(id: &str, p: &Pending) -> EventInput {
        EventInput {
            kind: EventType::ControlFailed,
            rig: p.rig.clone(),
            actor: "campd".into(),
            bead: p.bead.clone(),
            data: serde_json::json!({
                "session": p.session,
                "request_id": id,
                "verb": p.verb,
                "cause": "session_ended",
                "reason": format!(
                    "the session {} ended with an unanswered control request \
                     (request_id {id}, {}). The most likely story is that the interrupt \
                     WORKED and the worker died before flushing its ack — but camp does \
                     not know that, so it says what it does know rather than nothing \
                     (invariant 3)",
                    p.session, p.verb
                ),
            }),
        }
    }

    /// G7: the session was disposed. Its still-PENDING rows are EXPIRED LOUDLY
    /// — never silently dropped — and its `answered`/`timed_out` rows are
    /// PRUNED, which is what bounds both maps by LIVE sessions.
    ///
    /// A late answer cannot arrive after disposal: the session is no longer
    /// tailed, so there is nothing left to re-read.
    pub fn forget_session(&mut self, session: &str, _now: Timestamp) -> Vec<EventInput> {
        let doomed: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, p)| p.session == session)
            .map(|(id, _)| id.clone())
            .collect();

        let mut events = Vec::new();
        for id in doomed {
            let Some(p) = self.pending.remove(&id) else {
                continue;
            };
            events.push(ControlRuntime::session_ended_fault(&id, &p));
        }

        // Prune the two SETTLED maps for this session — the other half of the
        // bound. Nothing is swallowed here: an `answered` row has already produced its
        // event, and a `timed_out` row already produced its `control.failed`.
        // What is dropped is the memory of them, and only for a session that no
        // longer exists.
        self.timed_out.retain(|_, p| p.session != session);
        self.answered.retain(|_, s| s != session);

        events
    }
}

/// What `rehydrate` rebuilt — and what it must SAY.
///
/// The events are not optional: a request whose session died while campd was down
/// can never be disposed, so `forget_session` never runs for it and `rehydrate` is
/// the only thing left that can give it a terminal event (§2.1).
pub struct Rehydrated {
    pub restored: usize,
    pub events: Vec<EventInput>,
}

/// A deadline as a Duration from now. Saturates at ZERO — a deadline in the
/// past is due NOW, never a negative timeout.
fn duration_until(deadline: Timestamp, now: Timestamp) -> Duration {
    let delta = deadline - now;
    if delta.is_negative() {
        return Duration::ZERO;
    }
    Duration::try_from(delta).unwrap_or(Duration::ZERO)
}

// ---------------------------------------------------------------------------
// cp-1 (§4.1/§4.4): the socket-verb handlers, and the inbound ingest.
//
// Every handler body lives HERE, so `event_loop.rs`'s new arms are one-line
// delegations. The event loop is the most contended file in the tree; a phase
// that puts its logic there makes the next phase's rebase a merge conflict.
// ---------------------------------------------------------------------------

/// §4.4's number. The per-subscriber outbound buffer cap.
pub const SUBSCRIBER_BUFFER_BYTES_DEFAULT: usize = 1024 * 1024;

/// Test-only override, the `CAMP_MAX_STREAM_BYTES` twin (read_channel.rs).
/// Production uses the default until `config.rs` gains a `[control]` field in a
/// phase that owns it. Fail fast: a malformed override is an error, never
/// silently ignored.
pub fn subscriber_buffer_bytes_from_env(default: usize) -> anyhow::Result<usize> {
    match std::env::var("CAMP_SUBSCRIBER_BUFFER_BYTES") {
        Ok(raw) => {
            let n: usize = raw
                .parse()
                .with_context(|| format!("CAMP_SUBSCRIBER_BUFFER_BYTES={raw:?} is not a usize"))?;
            if n == 0 {
                anyhow::bail!("CAMP_SUBSCRIBER_BUFFER_BYTES must be > 0");
            }
            Ok(n)
        }
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(v)) => {
            anyhow::bail!("CAMP_SUBSCRIBER_BUFFER_BYTES={v:?} is not valid UTF-8")
        }
    }
}

/// Loud is right; UNBOUNDED-loud is a self-DoS. A worker spraying malformed
/// control lines would otherwise drive one synchronous SQLite append per line,
/// on the event loop. This bounds the fault events ONE `ingest` call may emit
/// for ONE session.
///
/// It is a PER-CALL counter (a local map, reset at the top of every `ingest`),
/// not runtime state — hence "per wake". Past the cap, further faults for that
/// session are suppressed and the LAST event's `reason` names the suppressed
/// count. The count rides `reason` precisely so no new payload field is needed:
/// `ControlFailed` is `deny_unknown_fields`, and adding a field later would
/// break every event already in every ledger.
pub const MAX_FAULTS_PER_SESSION_PER_WAKE: usize = 8;

impl ControlRuntime {
    /// §4.1 `session.interrupt`. D1 (ACK-then-ASYNC) + D2 (deliver -> record ->
    /// respond). campd does NOT wait for the `control_response`: its loop is
    /// single-threaded, and blocking a handler on a filesystem-latency line is
    /// issue #55's wedge class. The answer returns on the read channel
    /// (`ingest`), survives a restart (`rehydrate`, B6), and a late answer
    /// appends a correction (C11).
    ///
    /// ORDERING, and what camp does NOT promise: an interrupt and a `send_turn`
    /// are both LINES IN THE SAME held stdin pipe, written in socket-arrival
    /// order. camp makes NO guarantee that an interrupt "cancels" a turn already
    /// queued ahead of it — a caller assuming that is assuming something camp
    /// does not promise. Two concurrent interrupts mint DISTINCT request_ids and
    /// produce two independent pending rows and two `control.responded`s; that
    /// is correct and needs no coordination.
    pub fn serve_interrupt(
        &mut self,
        session: &str,
        ledger: &mut Ledger,
        dispatcher: &mut Dispatcher,
        now: Timestamp,
    ) -> Response {
        // Bound the table AND the ledger: neither an overseer loop nor a hostile
        // local client may grow `pending` or append `session.interrupted`
        // without limit.
        if self.pending.len() >= MAX_PENDING_CONTROL_REQUESTS {
            return Response::Error {
                ok: false,
                error: format!(
                    "campd already has {} unanswered control requests outstanding (the \
                     MAX_PENDING_CONTROL_REQUESTS cap) — something is issuing interrupts \
                     faster than workers answer them",
                    self.pending.len()
                ),
            };
        }
        let request_id = new_request_id();
        let line = match (ParentMessage::Interrupt {
            request_id: request_id.clone(),
        })
        .to_line()
        {
            Ok(line) => line,
            Err(e) => {
                return Response::Error {
                    ok: false,
                    error: format!("building the interrupt: {e}"),
                };
            }
        };
        let (rig, bead) = dispatcher
            .child_info(session)
            .map(|(r, b)| (Some(r), Some(b)))
            .unwrap_or((None, None));

        match dispatcher.write_control(session, &line) {
            // D2: DELIVER -> RECORD. The ledger must not claim what was not
            // delivered, and the caller must not believe what the ledger lacks.
            ControlWrite::Delivered => match ledger.append(EventInput {
                kind: EventType::SessionInterrupted,
                rig: rig.clone(),
                actor: "campd".into(),
                bead: bead.clone(),
                data: serde_json::json!({"session": session, "request_id": request_id}),
            }) {
                Ok(_) => {
                    // G7: the rig/bead go INTO the pending row, so every fault
                    // this request may later produce (silence_timeout,
                    // ceiling_timeout, session_ended) carries the SAME
                    // provenance as the `session.interrupted` it answers.
                    self.track_pending(
                        request_id.clone(),
                        session.to_owned(),
                        "session.interrupt",
                        rig,
                        bead,
                        now,
                    );
                    Response::Interrupt {
                        ok: true,
                        request_id,
                    }
                }
                Err(e) => Response::Error {
                    ok: false,
                    error: format!(
                        "interrupt delivered into {session} but recording session.interrupted \
                         failed: {e}"
                    ),
                },
            },
            // There is NO resume path for an interrupt (unlike a turn): a worker
            // campd holds no pipe to CANNOT be interrupted, and pretending
            // otherwise would be a silent no-op. Loud — and NOT evented:
            // nothing happened, so there is no campd action to record
            // (invariant 3 records ACTIONS; a refused verb is the caller's error).
            ControlWrite::NoPipe => Response::Error {
                ok: false,
                error: format!(
                    "campd holds no stdin pipe for {session} — it is not a live campd-spawned \
                     worker (exited, released, attended, or adopted from a previous campd \
                     life), and there is no other way to interrupt a turn (control-plane \
                     spec §2.3)"
                ),
            },
            // C12 — the write was ATTEMPTED and FAILED, so bytes may already
            // have reached the pipe and `write_control` has torn it down. That
            // IS a campd action with a consequence — the worker just lost its
            // write channel — so it is BOTH an error to the caller AND a durable
            // fault (§2.1 loudness; invariant 3). Bounded: one socket request =>
            // one event, and the request_id is fresh, so a retrying caller
            // cannot dedupe-collide.
            ControlWrite::Failed(e) => {
                let reason = format!(
                    "writing an interrupt into {session}'s held stdin failed: {e}. The pipe may \
                     hold a torn partial line, so campd dropped it — this worker can no longer \
                     be sent turns or control messages, and patrol's stall ladder now owns it"
                );
                match ledger.append(EventInput {
                    kind: EventType::ControlFailed,
                    rig,
                    actor: "campd".into(),
                    bead,
                    data: serde_json::json!({
                        "session": session,
                        "request_id": request_id,
                        "verb": "session.interrupt",
                        // G5: the machine-readable cause. TERMINAL — the request
                        // never reached the worker, so no answer can ever arrive,
                        // and `rehydrate` must route this id to `answered`.
                        "cause": "write_failed",
                        "reason": reason,
                    }),
                }) {
                    Ok(_) => Response::Error {
                        ok: false,
                        error: reason,
                    },
                    // A failing append must not MASK the write failure being
                    // reported — carry both.
                    Err(append_err) => Response::Error {
                        ok: false,
                        error: format!(
                            "{reason} (and recording control.failed ALSO failed: {append_err})"
                        ),
                    },
                }
            }
        }
    }

    /// §4.1 `session.send_turn` (D4 — the `nudge` socket verb's replacement).
    /// Deliver -> record (`session.nudged`) -> respond. `NoPipe` is NOT an error
    /// here — unlike an interrupt, a turn HAS a resume path, and `via:"none"`
    /// is what routes the caller to it.
    pub fn serve_send_turn(
        &mut self,
        session: &str,
        text: &str,
        ledger: &mut Ledger,
        dispatcher: &mut Dispatcher,
    ) -> Response {
        match dispatcher.nudge_via_stdin(session, text) {
            NudgeOutcome::Delivered => {
                let (rig, bead) = dispatcher
                    .child_info(session)
                    .map(|(r, b)| (Some(r), Some(b)))
                    .unwrap_or((None, None));
                match ledger.append(EventInput {
                    kind: EventType::SessionNudged,
                    rig,
                    actor: "campd".into(),
                    bead,
                    data: serde_json::json!({
                        "session": session, "via": "stdin", "text": text,
                    }),
                }) {
                    Ok(_) => Response::SendTurn {
                        ok: true,
                        via: "stdin".into(),
                    },
                    // A post-delivery append failure surfaces to the caller: the
                    // ledger must not claim what was not delivered, and the
                    // caller must not believe what the ledger does not hold.
                    Err(e) => Response::Error {
                        ok: false,
                        error: format!(
                            "turn delivered into {session} but recording session.nudged \
                             failed: {e}"
                        ),
                    },
                }
            }
            NudgeOutcome::NoPipe => Response::SendTurn {
                ok: true,
                via: "none".into(),
            },
            NudgeOutcome::Failed(e) => Response::Error {
                ok: false,
                error: format!("stdin turn delivery to {session} failed: {e}"),
            },
        }
    }

    /// §4.1/§4.2/§4.3 `sessions.list`: every live session, BY NAME.
    ///
    /// Answered from the LEDGER's registry (`live_sessions`), NOT campd's child
    /// map: an ADOPTED worker from a previous campd life is a live session too,
    /// and a fleet view that could not see it would be lying by omission (§4.3).
    pub fn serve_sessions_list(
        &self,
        ledger: &Ledger,
        patrol: &PatrolRuntime,
        read_channel: &ReadChannelRuntime,
    ) -> Response {
        match self.fleet_model(ledger, patrol, read_channel) {
            Ok(sessions) => Response::SessionsList { ok: true, sessions },
            Err(e) => Response::Error {
                ok: false,
                error: format!("listing live sessions: {e}"),
            },
        }
    }

    /// §4.1/§4.3: the fleet — one `SessionInfo` per LIVE session, BY NAME, from the
    /// ledger registry (not campd's child map: an adopted worker is a live session
    /// too). The single definition shared by `sessions.list` and `fleet.subscribe`.
    pub fn fleet_model(
        &self,
        ledger: &Ledger,
        patrol: &PatrolRuntime,
        read_channel: &ReadChannelRuntime,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let rows = ledger.live_sessions()?;
        Ok(rows
            .into_iter()
            .map(|row| SessionInfo {
                // `last_activity` is the last complete line the session produced;
                // a session that has produced none has still WOKEN, and the wake
                // is the honest answer — never a zero or a null.
                last_activity: read_channel
                    .last_activity(&row.name)
                    .map(|t| t.to_string())
                    .unwrap_or(row.spawned_ts),
                // EXACTLY TWO VALUES in cp-1. The doc comment on SessionInfo
                // promises no third.
                state: if patrol.is_stalled(&row.name) {
                    "stalled".into()
                } else {
                    "working".into()
                },
                // §5.3: phase 3 owns the producer. cp-1 never flips this quietly
                // — a can_use_tool that arrives is a LOUD control.failed.
                blocked: false,
                name: row.name,
                agent: row.agent,
                rig: row.rig,
                bead: row.bead,
            })
            .collect())
    }

    /// §4.1/§4.4 `fleet.subscribe`: the hello. It REGISTERS and refreshes the cached
    /// model so the FIRST post-hello pump (event_loop) emits the full snapshot; it
    /// never writes — `respond()` writes the hello, then the loop pumps (B11).
    pub fn serve_fleet_subscribe(
        &mut self,
        token: Token,
        ledger: &Ledger,
        patrol: &PatrolRuntime,
        read_channel: &ReadChannelRuntime,
    ) -> Response {
        if self.subscribers.len() >= MAX_SUBSCRIBERS {
            return Response::Error {
                ok: false,
                error: format!(
                    "campd already has {MAX_SUBSCRIBERS} subscriptions open (the MAX_SUBSCRIBERS \
                     cap). Each holds up to {SUBSCRIBER_BUFFER_BYTES} bytes of outbound buffer."
                ),
            };
        }
        match self.fleet_model(ledger, patrol, read_channel) {
            Ok(model) => self.fleet_model = model,
            Err(e) => {
                return Response::Error {
                    ok: false,
                    error: format!("building the fleet model: {e}"),
                };
            }
        }
        self.next_subscription += 1;
        let id = format!("fleet-{}", self.next_subscription);
        self.subscribers.insert(
            token,
            Subscriber {
                id: id.clone(),
                out: OutBuf::new(),
                source: Source::Fleet(FleetSource::new()),
            },
        );
        Response::FleetSubscribed {
            ok: true,
            v: 1,
            subscription: id,
        }
    }

    /// True when at least one fleet subscriber is registered — the guard that keeps
    /// the model recompute off the hot path when nobody is watching.
    fn has_fleet_subscribers(&self) -> bool {
        self.subscribers
            .values()
            .any(|s| matches!(s.source, Source::Fleet(_)))
    }

    /// The INBOUND half: everything the read channel drained this wake.
    ///
    /// This is where a worker's answer to an interrupt actually lands — and
    /// where every other control message it can send is met with a decision
    /// rather than a shrug.
    pub fn ingest(
        &mut self,
        lines: &[StreamLine],
        dispatcher: &mut Dispatcher,
        now: Timestamp,
    ) -> Vec<EventInput> {
        let mut events: Vec<EventInput> = Vec::new();
        // Per-CALL, hence per-wake. Not runtime state.
        let mut faults: HashMap<String, usize> = HashMap::new();
        let mut refused_dialogs: HashSet<String> = HashSet::new();

        for sl in lines {
            // D7/C11 FIRST, for EVERY line: the session is producing output, so
            // its SILENCE deadline resets. (The G6 ceiling does not — nothing
            // resets that, which is the whole point of having it.)
            self.note_activity(&sl.session, now);

            match parse_worker_line(&sl.line) {
                Ok(WorkerMessage::ControlResponse {
                    request_id,
                    ok,
                    detail,
                }) => {
                    // B6/C11: `resolve` decides whether this is the answer, a
                    // correction, a true duplicate, or a fault.
                    if let Some(input) = self.resolve(&request_id, ok, detail) {
                        events.push(input);
                    }
                }
                Ok(WorkerMessage::RequestUserDialog { request_id }) => {
                    // §9: camp is not a human. Refuse — DETERMINISTICALLY — or
                    // the worker blocks forever holding a dispatch slot.
                    // Deduped per request_id: a worker re-asking the same id
                    // must not append an event per line.
                    if !refused_dialogs.insert(request_id.clone()) {
                        continue;
                    }
                    let outcome = match (ParentMessage::DialogRefusal {
                        request_id: request_id.clone(),
                    })
                    .to_line()
                    {
                        Ok(line) => match dispatcher.write_control(&sl.session, &line) {
                            ControlWrite::Delivered => "the refusal was delivered".to_owned(),
                            ControlWrite::NoPipe => {
                                "campd holds no stdin pipe for it, so the refusal could NOT be \
                                 delivered and the worker is now blocked forever — kill it"
                                    .to_owned()
                            }
                            ControlWrite::Failed(e) => {
                                format!(
                                    "delivering the refusal FAILED ({e}) — the worker is now \
                                         blocked forever; kill it"
                                )
                            }
                        },
                        Err(e) => format!("building the refusal failed: {e}"),
                    };
                    let input = EventInput {
                        kind: EventType::ControlFailed,
                        rig: None,
                        actor: "campd".into(),
                        bead: None,
                        data: serde_json::json!({
                            "session": sl.session,
                            "request_id": request_id,
                            "cause": "dialog_refused",
                            "reason": format!(
                                "the worker asked for an interactive dialog (§9). camp is not a \
                                 human and has no dialog to show, so it answers every one with a \
                                 deterministic refusal: {outcome}"
                            ),
                        }),
                    };
                    push_fault(&mut events, &mut faults, &sl.session, input);
                }
                Ok(WorkerMessage::CanUseTool {
                    request_id,
                    tool_name,
                }) => {
                    // §5.3.1: STRUCTURALLY UNREACHABLE in cp-1 — camp does not
                    // pass `--permission-prompt-tool`. If it arrives anyway,
                    // something is badly wrong, and camp says so plainly rather
                    // than quietly flipping a `blocked` bit. cp-1 takes NO
                    // automatic action: phase 3 owns both the answer and §5.3.2's
                    // slot rule.
                    let input = EventInput {
                        kind: EventType::ControlFailed,
                        rig: None,
                        actor: "campd".into(),
                        bead: None,
                        data: serde_json::json!({
                            "session": sl.session,
                            "request_id": request_id,
                            "cause": "permission_unanswerable",
                            "reason": format!(
                                "the worker asked permission to use {tool_name:?}, and cp-1 \
                                 CANNOT answer a can_use_tool (the permission plane is phase 3). \
                                 This flow should be structurally unreachable — camp does not \
                                 pass --permission-prompt-tool — so its arrival is itself the \
                                 fault. The worker is now BLOCKED FOREVER waiting for an answer \
                                 that will never come, holding a dispatch slot: stop it with \
                                 `camp stop {}`",
                                sl.session
                            ),
                        }),
                    };
                    push_fault(&mut events, &mut faults, &sl.session, input);
                }
                // D6": subscribers are fed from the FILE by `pump`, never from
                // here. A stream line has no control-plane effect beyond the
                // activity stamp above.
                Ok(WorkerMessage::Stream(_)) => {}
                Err(e) => {
                    // §2.1: an unrecognized control message is a loud fault.
                    //
                    // cp-0's `drain_one` hands over only lines it ALREADY
                    // parsed as JSON (the `Ok(_v)` arm) and surfaces non-JSON
                    // lines separately as `patrol.degraded` — so `ingest` never
                    // double-reports. Do not add a guard.
                    let input = EventInput {
                        kind: EventType::ControlFailed,
                        rig: None,
                        actor: "campd".into(),
                        bead: None,
                        data: serde_json::json!({
                            "session": sl.session,
                            "cause": "unparsable",
                            "reason": format!(
                                "camp could not understand a control message from {}: {}. The \
                                 line was: {}",
                                sl.session,
                                e.reason,
                                truncate(&e.line, 400),
                            ),
                        }),
                    };
                    push_fault(&mut events, &mut faults, &sl.session, input);
                }
            }
        }

        // The suppressed count is known only once every line is seen, so it is
        // appended to the LAST fault this wake produced for each capped session
        // rather than guessed at the 8th.
        for (session, n) in faults {
            if n <= MAX_FAULTS_PER_SESSION_PER_WAKE {
                continue;
            }
            let suppressed = n - MAX_FAULTS_PER_SESSION_PER_WAKE;
            if let Some(last) = events
                .iter_mut()
                .rev()
                .find(|e| e.data["session"] == session && e.kind == EventType::ControlFailed)
                && let Some(reason) = last.data["reason"].as_str()
            {
                let amended = format!(
                    "{reason} — and {suppressed} further control-message fault(s) for this \
                     session this wake were SUPPRESSED (the MAX_FAULTS_PER_SESSION_PER_WAKE cap: \
                     loud is right, unbounded-loud is a self-DoS)"
                );
                last.data["reason"] = serde_json::Value::String(amended);
            }
        }

        events
    }
}

/// Count a fault against its session's per-wake budget, and emit it only while
/// the budget lasts. Loud is right; UNBOUNDED-loud is a self-DoS — a worker
/// spraying malformed control lines would otherwise drive one synchronous
/// SQLite append per line, on the event loop.
fn push_fault(
    events: &mut Vec<EventInput>,
    faults: &mut HashMap<String, usize>,
    session: &str,
    input: EventInput,
) {
    let n = faults.entry(session.to_owned()).or_insert(0);
    *n += 1;
    if *n <= MAX_FAULTS_PER_SESSION_PER_WAKE {
        events.push(input);
    }
}

/// Bound a line quoted into a fault's `reason`: a worker can write a 256 MiB
/// line, and the ledger is not the place to store it.
fn truncate(line: &str, max: usize) -> String {
    if line.len() <= max {
        return line.to_owned();
    }
    let mut end = max;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes total)", &line[..end], line.len())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::ledger::Ledger;

    /// A ledger in a temp dir — the camp-core unit-test mold.
    fn temp_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        (dir, ledger)
    }

    fn t0() -> Timestamp {
        "2026-07-13T12:00:00Z".parse().unwrap()
    }

    fn secs(t: Timestamp, n: i64) -> Timestamp {
        t + SignedDuration::from_secs(n)
    }

    /// Track one pending interrupt, exactly as `serve_interrupt` does.
    fn track(rt: &mut ControlRuntime, id: &str, session: &str, now: Timestamp) {
        rt.track_pending(
            id.to_owned(),
            session.to_owned(),
            "session.interrupt",
            Some("gc".into()),
            Some("gc-1".into()),
            now,
        );
    }

    fn data(input: &EventInput) -> &serde_json::Value {
        &input.data
    }

    /// Seed a LIVE session, so `rehydrate`'s liveness filter keeps its requests.
    /// (`session.woke`'s payload key is `name`, and `woke_actor` is derived from
    /// the event's `actor` column — not a payload field.)
    fn seed_live_session(ledger: &mut Ledger, session: &str) {
        ledger
            .append(EventInput {
                kind: EventType::SessionWoke,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({"name": session, "agent": "dev"}),
            })
            .unwrap();
    }

    fn seed_interrupted(ledger: &mut Ledger, session: &str, id: &str) {
        ledger
            .append(EventInput {
                kind: EventType::SessionInterrupted,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"session": session, "request_id": id}),
            })
            .unwrap();
    }

    // ======== Task 3: the pending table ===================================

    /// Invariant 1: a deadline is armed only while something is pending. An
    /// empty table arms NOTHING, so an idle campd blocks forever.
    #[test]
    fn a_pending_request_arms_a_deadline_and_an_empty_table_arms_none() {
        let mut rt = ControlRuntime::new(1024);
        assert_eq!(rt.poll_timeout(t0()), None, "an idle campd must not tick");

        track(&mut rt, "camp-1", "t/dev/1", t0());
        assert_eq!(
            rt.poll_timeout(t0()).expect("a pending request arms"),
            CONTROL_RESPONSE_TIMEOUT
        );
        // Part-way there, what is armed is the REMAINING time.
        assert_eq!(
            rt.poll_timeout(secs(t0(), 10)).unwrap(),
            Duration::from_secs(20)
        );
        // Past the deadline it is due NOW — never negative.
        assert_eq!(rt.poll_timeout(secs(t0(), 99)).unwrap(), Duration::ZERO);
    }

    /// §2.1: "a control response that never arrives is an evented,
    /// operator-visible fault — never a swallowed timeout."
    #[test]
    fn a_control_response_that_never_arrives_becomes_a_durable_fault() {
        let mut rt = ControlRuntime::new(1024);
        track(&mut rt, "camp-1", "t/dev/1", t0());

        assert!(
            rt.expire_pending(secs(t0(), 29)).is_empty(),
            "nothing expires before the deadline"
        );

        let events = rt.expire_pending(secs(t0(), 31));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventType::ControlFailed);
        assert_eq!(data(&events[0])["cause"], "silence_timeout");
        assert_eq!(data(&events[0])["request_id"], "camp-1");
        assert_eq!(data(&events[0])["session"], "t/dev/1");
        assert_eq!(data(&events[0])["verb"], "session.interrupt");
        // G7: the fault carries the SAME provenance as the interrupt it answers.
        assert_eq!(events[0].rig.as_deref(), Some("gc"));
        assert_eq!(events[0].bead.as_deref(), Some("gc-1"));

        // The row is REMOVED, so the fault is raised EXACTLY ONCE — not once
        // per wake, forever.
        assert!(
            rt.expire_pending(secs(t0(), 999)).is_empty(),
            "an expired request must not re-fault on every wake"
        );
        assert_eq!(rt.poll_timeout(secs(t0(), 31)), None);
    }

    #[test]
    fn a_matching_control_response_resolves_the_pending_request() {
        let mut rt = ControlRuntime::new(1024);
        track(&mut rt, "camp-1", "t/dev/1", t0());

        let event = rt
            .resolve("camp-1", true, "{\"still_queued\":[]}".into())
            .expect("a matching response resolves");
        assert_eq!(event.kind, EventType::ControlResponded);
        assert_eq!(data(&event)["request_id"], "camp-1");
        assert_eq!(data(&event)["session"], "t/dev/1");
        assert_eq!(data(&event)["ok"], true);
        assert_eq!(data(&event)["late"], false);
        assert_eq!(data(&event)["verb"], "session.interrupt");
        assert_eq!(event.rig.as_deref(), Some("gc"));

        // Resolved => nothing armed, nothing left to expire.
        assert_eq!(rt.poll_timeout(t0()), None);
        assert!(rt.expire_pending(secs(t0(), 999)).is_empty());
    }

    /// B6: a restart neither LIES (inventing a fault for a request that was
    /// answered) nor FORGETS (dropping one that was not).
    #[test]
    fn a_restart_across_an_in_flight_interrupt_neither_lies_nor_forgets() {
        let (_dir, mut ledger) = temp_ledger();
        seed_live_session(&mut ledger, "t/dev/1");
        seed_interrupted(&mut ledger, "t/dev/1", "camp-1");
        seed_interrupted(&mut ledger, "t/dev/1", "camp-2");
        ledger
            .append(EventInput {
                kind: EventType::ControlResponded,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "session": "t/dev/1", "request_id": "camp-1",
                    "verb": "session.interrupt", "ok": true, "detail": "", "late": false,
                }),
            })
            .unwrap();

        let mut rt = ControlRuntime::new(1024);
        assert_eq!(
            rt.rehydrate(&ledger, t0()).unwrap().restored,
            1,
            "only the UNANSWERED request is restored as pending"
        );

        // It does not LIE: a re-read of the answered response is a TRUE
        // duplicate, so it appends nothing.
        assert!(
            rt.resolve("camp-1", true, "x".into()).is_none(),
            "an already-answered id must not append a second control.responded"
        );

        // It does not FORGET: the orphan still expires, loudly.
        let events = rt.expire_pending(secs(t0(), 31));
        assert_eq!(events.len(), 1);
        assert_eq!(data(&events[0])["request_id"], "camp-2");
        assert_eq!(data(&events[0])["cause"], "silence_timeout");
    }

    /// §2.1: a `control_response` for an id camp NEVER SENT is a fault, not a
    /// shrug — it means the wire is carrying something camp does not understand.
    #[test]
    fn a_control_response_for_a_never_sent_request_id_is_a_fault() {
        let mut rt = ControlRuntime::new(1024);
        let event = rt
            .resolve("camp-ghost", true, "x".into())
            .expect("an unknown id must produce a fault, never silence");
        assert_eq!(event.kind, EventType::ControlFailed);
        assert_eq!(data(&event)["cause"], "unknown_request");
        assert_eq!(data(&event)["request_id"], "camp-ghost");
    }

    /// D7/C11: THE DEADLINE MEASURES SILENCE, NOT ELAPSED TIME.
    ///
    /// A worker producing output is ALIVE, and its interrupt may simply be
    /// queued behind its turn. Whether the CLI reads stdin mid-turn is
    /// genuinely unknown — and a SILENCE deadline makes that question
    /// non-load-bearing for correctness, because `control.failed` then means
    /// "the session went QUIET with a request unanswered", which is a real
    /// fault under either semantics.
    #[test]
    fn session_activity_resets_a_pending_control_deadline() {
        let mut rt = ControlRuntime::new(1024);
        track(&mut rt, "camp-1", "t/dev/1", t0());

        // The worker streams a line at T+20.
        rt.note_activity("t/dev/1", secs(t0(), 20));

        // T+31 would have expired an ELAPSED-time deadline. It must NOT expire
        // a SILENCE deadline: the worker was talking 11 seconds ago.
        assert!(
            rt.expire_pending(secs(t0(), 31)).is_empty(),
            "a streaming worker is ALIVE — its silence deadline resets"
        );

        // 30s of actual SILENCE after that line: NOW it is a fault.
        let events = rt.expire_pending(secs(t0(), 20 + 31));
        assert_eq!(events.len(), 1);
        assert_eq!(data(&events[0])["cause"], "silence_timeout");

        // Activity on ANOTHER session must never keep this one alive.
        let mut rt = ControlRuntime::new(1024);
        track(&mut rt, "camp-1", "t/dev/1", t0());
        rt.note_activity("t/dev/2", secs(t0(), 20));
        assert_eq!(
            rt.expire_pending(secs(t0(), 31)).len(),
            1,
            "another session's chatter must not reset this request's deadline"
        );
    }

    /// C11: a LATE answer is NEW INFORMATION, not a duplicate. It says the
    /// fault campd already appended was PREMATURE — so campd appends the
    /// correction. Rev 2 of this plan discarded the answer; this test makes
    /// that impossible.
    #[test]
    fn a_late_control_response_after_the_deadline_appends_a_correction() {
        let mut rt = ControlRuntime::new(1024);
        track(&mut rt, "camp-1", "t/dev/1", t0());

        let faults = rt.expire_pending(secs(t0(), 31));
        assert_eq!(data(&faults[0])["cause"], "silence_timeout");

        // The worker answers anyway, later.
        let event = rt
            .resolve("camp-1", true, "{\"still_queued\":[]}".into())
            .expect("a late answer must CORRECT the fault, never be swallowed");
        assert_eq!(event.kind, EventType::ControlResponded);
        assert_eq!(data(&event)["late"], true, "the correction must say so");
        assert_eq!(data(&event)["ok"], true);
        assert_eq!(data(&event)["request_id"], "camp-1");
        assert!(
            data(&event)["detail"]
                .as_str()
                .unwrap()
                .contains("PREMATURE"),
            "the correction must NAME the fault it corrects: {:?}",
            data(&event)["detail"]
        );

        // ...and exactly ONCE. A second re-read is a true duplicate.
        assert!(rt.resolve("camp-1", true, "x".into()).is_none());
    }

    /// G6/A3: A CHATTY WORKER THAT NEVER ANSWERS STILL FAULTS.
    ///
    /// A silence deadline alone can be pushed forward FOREVER by a worker that
    /// keeps talking — so an interrupt the CLI never processes would fault
    /// NEVER: §2.1's swallowed timeout, straight through the front door. And
    /// there is no backstop: patrol's stall ladder is ALSO activity-driven, so
    /// a chatty worker is never stalled either. Both safety nets are the same
    /// net, with a hole in exactly this shape. Hence the ABSOLUTE CEILING,
    /// which NOTHING resets.
    #[test]
    fn a_chatty_worker_that_never_answers_still_faults() {
        let mut rt = ControlRuntime::new(1024);
        track(&mut rt, "camp-1", "t/dev/1", t0());

        // A line every 5 seconds, all the way PAST THE CEILING (300s — not 3x
        // the 30s timeout, i.e. 90s, which never reaches the ceiling and so
        // could not observe what this test asserts).
        let mut faulted_at: Option<i64> = None;
        let mut n = 5;
        while n <= 400 {
            let now = secs(t0(), n);
            rt.note_activity("t/dev/1", now);
            let events = rt.expire_pending(now);
            if !events.is_empty() {
                assert_eq!(events.len(), 1);
                // The CEILING fired — NOT the silence deadline. The worker
                // never went quiet for 30s, not once.
                assert_eq!(
                    data(&events[0])["cause"],
                    "ceiling_timeout",
                    "a chatty worker's fault must name its TRUE cause (invariant 3)"
                );
                assert!(
                    data(&events[0])["reason"]
                        .as_str()
                        .unwrap()
                        .contains("never answered"),
                    "the reason must say the session produced output but never answered"
                );
                faulted_at = Some(n);
                break;
            }
            n += 5;
        }
        let at = faulted_at.expect(
            "a worker that streams forever and never answers MUST still fault — \
             without the ceiling NOTHING ever fires and §2.1's timeout is swallowed",
        );
        assert!(
            (300..=305).contains(&at),
            "the ceiling is 300s from created_at; it fired at {at}s"
        );
    }

    /// VT-1 — THE DELAYED WAKE. The `cause` discriminant, defended at last.
    ///
    /// `expire_pending` derives the cause by comparing THE TWO BOUNDS
    /// (`p.deadline <= ceiling`), NEVER either against `now`. A `now`-derived
    /// implementation (`if now < ceiling { silence } else { ceiling }`) is
    /// indistinguishable on every ordinary wake — and a mutation to it passed the
    /// ENTIRE suite, green, because **no test performed a wake past BOTH bounds.**
    ///
    /// A delayed wake is the ONLY observation that separates them, and it is
    /// reachable: campd runs adoption probes and `exec_timeout`-bounded git/`pgrep`
    /// subprocesses INLINE on the event loop, and a suspended laptop does it
    /// trivially. Under the `now`-derived version a pure SILENCE timeout is reported
    /// as `ceiling_timeout`, telling the operator "the session produced output for 5
    /// minutes" when it went quiet immediately — invariant 3's false cause.
    ///
    /// BOTH directions are pinned, because only the pair distinguishes the two
    /// implementations: at a very late `now`, `now` is past both bounds in BOTH
    /// cases, so a `now`-derived cause is the SAME for both — and it must not be.
    #[test]
    fn a_delayed_wake_still_names_the_bound_that_actually_expired() {
        // (a) SILENCE: the worker went quiet immediately and never came back. The
        //     silence bound fired at t+30 — LONG before the ceiling at t+300.
        //     campd does not wake until t+400.
        let mut rt = ControlRuntime::new(1024);
        track(&mut rt, "camp-quiet", "t/dev/1", t0());

        let events = rt.expire_pending(secs(t0(), 400));
        assert_eq!(events.len(), 1);
        assert_eq!(
            data(&events[0])["cause"],
            "silence_timeout",
            "the SILENCE bound is what expired (at t+30). A cause derived from `now` \
             would say `ceiling_timeout` here — and tell the operator the session \
             produced output for five minutes when it went quiet immediately \
             (invariant 3: an event must name its TRUE cause)"
        );

        // (b) CEILING: the worker never stopped talking, so its silence deadline was
        //     pushed forward to t+295 and NEVER fired. The ceiling at t+300 is what
        //     expired. campd, again, does not wake until t+400.
        let mut rt = ControlRuntime::new(1024);
        track(&mut rt, "camp-chatty", "t/dev/1", t0());
        let mut n = 5;
        while n <= 290 {
            rt.note_activity("t/dev/1", secs(t0(), n));
            n += 5;
        }

        let events = rt.expire_pending(secs(t0(), 400));
        assert_eq!(events.len(), 1);
        assert_eq!(
            data(&events[0])["cause"],
            "ceiling_timeout",
            "the CEILING is what expired — the session never went quiet for 30s"
        );

        // The pair is the point: at t+400 `now` is past BOTH bounds in BOTH cases, so
        // any cause derived from `now` gives the SAME answer to both — and the two
        // faults are NOT the same fault.
    }

    /// G5: THE SEAM NOTHING EXERCISED — a restart across a TIMED-OUT interrupt.
    ///
    /// Rev 3 routed EVERY `control.failed` into `answered`, so a worker's real
    /// answer — arriving after the restart — resolved to `None` and DIED WITH
    /// IT. The `cause` discriminant is what makes the routing possible at all;
    /// this test is what proves it is used.
    #[test]
    fn a_restart_across_a_timed_out_interrupt_still_appends_the_correction() {
        let (_dir, mut ledger) = temp_ledger();
        seed_live_session(&mut ledger, "t/dev/1");

        let seed_failed = |ledger: &mut Ledger, id: &str, cause: &str| {
            seed_interrupted(ledger, "t/dev/1", id);
            ledger
                .append(EventInput {
                    kind: EventType::ControlFailed,
                    rig: Some("gc".into()),
                    actor: "campd".into(),
                    bead: Some("gc-1".into()),
                    data: serde_json::json!({
                        "session": "t/dev/1", "request_id": id,
                        "verb": "session.interrupt", "cause": cause, "reason": "seeded",
                    }),
                })
                .unwrap();
        };
        seed_failed(&mut ledger, "camp-timed-out", "silence_timeout");
        seed_failed(&mut ledger, "camp-write-failed", "write_failed");

        let mut rt = ControlRuntime::new(1024);
        rt.rehydrate(&ledger, t0()).unwrap();

        // A TIMED-OUT id: a late answer is still NEW INFORMATION.
        let event = rt
            .resolve("camp-timed-out", true, "{}".into())
            .expect("a late answer to a TIMED-OUT request must correct the fault");
        assert_eq!(event.kind, EventType::ControlResponded);
        assert_eq!(
            data(&event)["late"],
            true,
            "rev 3 returned None here — and the worker's answer died with the restart"
        );

        // THE CONVERSE: a TERMINAL cause can never be corrected. The request
        // never reached the worker, so a response for it is not "late" — it is
        // an answer to something that was never asked.
        let event = rt.resolve("camp-write-failed", true, "{}".into());
        assert!(
            event.is_none(),
            "a write_failed id is TERMINAL — a stray response for it is a \
             duplicate, never a correction: {event:?}"
        );
    }

    /// G7: the MOST LIKELY real scenario — the interrupt worked, and the worker
    /// died before flushing its ack. The request must NEVER vanish with no event.
    #[test]
    fn a_worker_that_exits_before_answering_still_faults_loudly() {
        let mut rt = ControlRuntime::new(1024);
        // An already-ANSWERED id and an already-TIMED-OUT id, both on the
        // session we are about to forget: both must be PRUNED, and neither may
        // produce an event (nothing further happened to them).
        track(&mut rt, "camp-3", "t/dev/1", t0());
        rt.resolve("camp-3", true, "x".into()).unwrap();
        track(&mut rt, "camp-4", "t/dev/1", t0());
        assert_eq!(rt.expire_pending(secs(t0(), 31)).len(), 1);

        // The two STILL-PENDING requests are tracked AFTER that expiry, so the
        // expiry above cannot have taken them with it.
        track(&mut rt, "camp-1", "t/dev/1", secs(t0(), 31));
        track(&mut rt, "camp-2", "t/dev/2", secs(t0(), 31));

        let events = rt.forget_session("t/dev/1", secs(t0(), 40));
        assert_eq!(
            events.len(),
            1,
            "exactly ONE fault: the still-PENDING request. The answered and \
             timed-out rows are pruned silently — nothing further happened to them"
        );
        assert_eq!(events[0].kind, EventType::ControlFailed);
        assert_eq!(data(&events[0])["cause"], "session_ended");
        assert_eq!(data(&events[0])["request_id"], "camp-1");
        assert_eq!(data(&events[0])["session"], "t/dev/1");
        assert_eq!(events[0].rig.as_deref(), Some("gc"));
        assert_eq!(events[0].bead.as_deref(), Some("gc-1"));

        // Every map is now empty FOR THAT SESSION — which is exactly what bounds
        // them by LIVE sessions. camp-3's `answered` row is gone, so a stray
        // response for it now reads as an unknown request; that is correct, the
        // session is gone.
        let stray = rt.resolve("camp-3", true, "x".into()).unwrap();
        assert_eq!(stray.kind, EventType::ControlFailed);
        assert_eq!(data(&stray)["cause"], "unknown_request");

        // ...and the OTHER session is untouched: its request still expires on
        // its own schedule.
        let events = rt.expire_pending(secs(t0(), 31 + 31));
        assert_eq!(events.len(), 1);
        assert_eq!(data(&events[0])["request_id"], "camp-2");
    }

    // ======== Task 1: the wire ============================================

    const INTERRUPT_REQUEST: &str =
        include_str!("../../tests/fixtures/control/interrupt_request.json");
    const CONTROL_RESPONSE_SUCCESS: &str =
        include_str!("../../tests/fixtures/control/control_response_success.json");
    const CONTROL_RESPONSE_ERROR: &str =
        include_str!("../../tests/fixtures/control/control_response_error.json");
    const CAN_USE_TOOL_REQUEST: &str =
        include_str!("../../tests/fixtures/control/can_use_tool_request.json");
    const REQUEST_USER_DIALOG_REQUEST: &str =
        include_str!("../../tests/fixtures/control/request_user_dialog_request.json");
    const DIALOG_REFUSAL_RESPONSE: &str =
        include_str!("../../tests/fixtures/control/dialog_refusal_response.json");
    const PERMISSION_ALLOW_RESPONSE: &str =
        include_str!("../../tests/fixtures/control/permission_allow_response.json");
    const PERMISSION_DENY_RESPONSE: &str =
        include_str!("../../tests/fixtures/control/permission_deny_response.json");
    const USER_TURN: &str = include_str!("../../tests/fixtures/control/user_turn.json");
    const STREAM_ASSISTANT: &str =
        include_str!("../../tests/fixtures/control/stream_assistant.json");

    /// Every shape camp SENDS is byte-equal to its fixture.
    ///
    /// Byte equality — not `Value` equality — is the point. The peer is a
    /// minified JS bundle whose parser we do not control, and camp's own
    /// `send_turn` bytes have shipped since Phase 8. `ParentMessage` is
    /// therefore built from `#[derive(Serialize)]` STRUCTS, whose field order
    /// is DECLARATION order, and never from `serde_json::json!`, whose key
    /// order is a `BTreeMap`'s (alphabetical).
    ///
    /// `user_turn.json` looks odd — `{"message":{...},"type":"user"}` — and
    /// that is CORRECT: `spawn::user_message` uses `json!`, so serde_json
    /// (1.0.150, no `preserve_order`) sorts its keys. Those are the bytes
    /// every production dispatch has always sent and the CLI has always
    /// accepted (probe P2). The fixture pins the ACTUAL output, so a later
    /// "tidy-up" of `user_message` into a struct — which would change the
    /// wire — turns this test RED. That is exactly what it is for.
    #[test]
    fn parent_messages_serialize_to_the_pinned_fixture_bytes() {
        let interrupt = ParentMessage::Interrupt {
            request_id: "camp-fixture-1".into(),
        }
        .to_line()
        .unwrap();
        assert_eq!(
            interrupt,
            format!("{INTERRUPT_REQUEST}\n"),
            "the interrupt camp sends must be byte-equal to its fixture"
        );

        let refusal = ParentMessage::DialogRefusal {
            request_id: "cli-fixture-3".into(),
        }
        .to_line()
        .unwrap();
        assert_eq!(
            refusal,
            format!("{DIALOG_REFUSAL_RESPONSE}\n"),
            "the dialog refusal camp sends must be byte-equal to its fixture"
        );

        // C1: the fixture is the ACTUAL output of the shipped code path.
        assert_eq!(
            crate::daemon::spawn::user_message("status?"),
            format!("{USER_TURN}\n"),
            "spawn::user_message's bytes are pinned — do not 'tidy' it into a struct"
        );
    }

    /// The order-independent guard: byte equality above could be satisfied by
    /// a fixture that is semantically wrong. This asserts the SHAPE too.
    #[test]
    fn parent_messages_are_semantically_equal_to_their_fixtures() {
        let interrupt = ParentMessage::Interrupt {
            request_id: "camp-fixture-1".into(),
        }
        .to_line()
        .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&interrupt).unwrap();
        let pinned: serde_json::Value = serde_json::from_str(INTERRUPT_REQUEST).unwrap();
        assert_eq!(sent, pinned);
        assert_eq!(sent["type"], "control_request");
        assert_eq!(sent["request"]["subtype"], "interrupt");

        let refusal = ParentMessage::DialogRefusal {
            request_id: "cli-fixture-3".into(),
        }
        .to_line()
        .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&refusal).unwrap();
        let pinned: serde_json::Value = serde_json::from_str(DIALOG_REFUSAL_RESPONSE).unwrap();
        assert_eq!(sent, pinned);
        assert_eq!(sent["response"]["subtype"], "error");
    }

    /// All four inbound shapes parse from the pinned fixtures.
    #[test]
    fn worker_messages_parse_from_the_pinned_fixtures() {
        match parse_worker_line(CONTROL_RESPONSE_SUCCESS).unwrap() {
            WorkerMessage::ControlResponse {
                request_id,
                ok,
                detail,
            } => {
                assert_eq!(request_id, "camp-fixture-1");
                assert!(ok);
                // The success detail is the inner `response` object verbatim.
                assert!(detail.contains("still_queued"), "detail was {detail:?}");
            }
            other => panic!("expected a ControlResponse, got {other:?}"),
        }

        match parse_worker_line(CONTROL_RESPONSE_ERROR).unwrap() {
            WorkerMessage::ControlResponse {
                request_id,
                ok,
                detail,
            } => {
                assert_eq!(request_id, "camp-fixture-1");
                assert!(!ok);
                // The `error` KEY is the verified one (recovered from the
                // bundle: `response:{subtype:"error",request_id:…,error:…}`).
                assert_eq!(detail, "no turn in progress");
            }
            other => panic!("expected a ControlResponse, got {other:?}"),
        }

        match parse_worker_line(CAN_USE_TOOL_REQUEST).unwrap() {
            WorkerMessage::CanUseTool {
                request_id,
                tool_name,
            } => {
                assert_eq!(request_id, "cli-fixture-2");
                assert_eq!(tool_name, "Bash");
            }
            other => panic!("expected a CanUseTool, got {other:?}"),
        }

        match parse_worker_line(REQUEST_USER_DIALOG_REQUEST).unwrap() {
            WorkerMessage::RequestUserDialog { request_id } => {
                assert_eq!(request_id, "cli-fixture-3");
            }
            other => panic!("expected a RequestUserDialog, got {other:?}"),
        }
    }

    /// C9: camp may NEVER depend on its `can_use_tool` fixture being
    /// COMPLETE. A fixed-width grep of a minified bundle cannot prove key
    /// completeness — and a second construction site in the very same bundle
    /// adds four keys the fixture does not carry. So the envelope is
    /// deliberately NOT `deny_unknown_fields`, and this test is what stops
    /// someone "tightening" it later.
    #[test]
    fn can_use_tool_with_unknown_extra_keys_still_parses() {
        let line = r#"{"type":"control_request","request_id":"cli-fixture-2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{},"permission_suggestions":["allow"],"blocked_path":"/etc","decision_reason":{"type":"rule"},"decision_reason_type":"rule","classifier_approvable":true,"agent_id":"a1","future_key":"a key that does not exist yet"}}"#;
        match parse_worker_line(line).unwrap() {
            WorkerMessage::CanUseTool {
                request_id,
                tool_name,
            } => {
                assert_eq!(request_id, "cli-fixture-2");
                assert_eq!(tool_name, "Bash");
            }
            other => panic!("unknown keys must not break the parse; got {other:?}"),
        }
    }

    /// D3: the transparent stream surface. A non-control line is handed back
    /// VERBATIM and never faults — camp does not interpret the worker's
    /// words, it only routes them.
    #[test]
    fn non_control_stream_lines_pass_through_verbatim_and_never_fault() {
        for line in [
            STREAM_ASSISTANT,
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"result","subtype":"success","is_error":false}"#,
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
        ] {
            match parse_worker_line(line).unwrap() {
                WorkerMessage::Stream(passed) => assert_eq!(
                    passed, line,
                    "a stream line must pass through byte-for-byte"
                ),
                other => panic!("{line} must be a Stream, got {other:?}"),
            }
        }
    }

    /// §2.1: "an unrecognized control message … is an evented,
    /// operator-visible fault — never a swallowed timeout."
    ///
    /// The last case is D3's PREFIX rule and it is the load-bearing one: a
    /// `control_notify` does not exist today. If the CLI ever adds one, camp
    /// must FAULT on it — not forward its contents to a subscriber as though
    /// the worker had said them.
    #[test]
    fn an_unrecognized_control_message_is_a_loud_error() {
        for line in [
            // an unknown control_request subtype
            r#"{"type":"control_request","request_id":"x","request":{"subtype":"set_model"}}"#,
            // an unknown control_response subtype
            r#"{"type":"control_response","response":{"subtype":"weird","request_id":"x"}}"#,
            // a control_request with no request_id
            r#"{"type":"control_request","request":{"subtype":"interrupt"}}"#,
            // a control_response with no request_id
            r#"{"type":"control_response","response":{"subtype":"success"}}"#,
            // not JSON at all
            "this is not json",
            // a control message camp has never heard of
            r#"{"type":"control_cancel_request","request_id":"x"}"#,
            // THE PREFIX RULE: a type that does not exist yet
            r#"{"type":"control_notify","request_id":"x","note":"anything"}"#,
        ] {
            let err = parse_worker_line(line)
                .expect_err("an unrecognized control message must be a loud error");
            assert_eq!(err.line, line, "the error must carry the offending line");
            assert!(!err.reason.is_empty(), "the error must name a reason");
        }
    }

    /// C10: cp-3's OUTBOUND permission bytes, pinned HERE so they cannot rot
    /// before cp-3 arrives. cp-1 does not send them — but it does hand phase
    /// 3 recovered bytes instead of a guess, and the CLI's own validator
    /// string is the contract they are checked against:
    ///
    ///   Expected {behavior: 'allow', updatedInput?: object}
    ///         or {behavior: 'deny', message: string}.
    #[test]
    fn the_permission_response_fixtures_match_the_cli_validator_contract() {
        for (fixture, expected_behavior) in [
            (PERMISSION_ALLOW_RESPONSE, "allow"),
            (PERMISSION_DENY_RESPONSE, "deny"),
        ] {
            let v: serde_json::Value = serde_json::from_str(fixture).unwrap();
            assert_eq!(v["type"], "control_response");
            assert_eq!(v["response"]["subtype"], "success");
            assert!(v["response"]["request_id"].is_string());
            let decision = &v["response"]["response"];
            match expected_behavior {
                "allow" => {
                    assert_eq!(decision["behavior"], "allow");
                    // `updatedInput` is OPTIONAL per the validator.
                    assert!(
                        decision.get("updatedInput").is_none_or(|u| u.is_object()),
                        "updatedInput, when present, must be an object"
                    );
                }
                _ => {
                    assert_eq!(decision["behavior"], "deny");
                    assert!(
                        decision["message"].is_string(),
                        "a deny MUST carry a `message` string"
                    );
                }
            }
        }
    }
    // ======== Task 8: session.subscribe ==================================

    use std::io::Read as _;

    /// A stream file holding `content`, plus its length (a natural `tail`).
    fn stream_file(content: &[u8]) -> (tempfile::TempDir, std::fs::File, u64) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t-dev-1.json");
        std::fs::write(&path, content).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        (dir, file, content.len() as u64)
    }

    /// Read the client end to EOF (the server `Conn` must be DROPPED for this to
    /// return). Used from a reader thread while the main thread pumps — a reader
    /// that gave up on the first read gap would stop MID-FRAME and see a truncated
    /// line, which is a bug in the test, not in `pump`.
    fn read_to_eof(client: &mut std::os::unix::net::UnixStream) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 65536];
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            match client.read(&mut chunk) {
                Ok(0) => break, // EOF: the server end was dropped
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() > deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => break,
            }
        }
        buf
    }

    fn frames_to_eof(client: &mut std::os::unix::net::UnixStream) -> Vec<serde_json::Value> {
        parse_frames(&read_to_eof(client))
    }

    fn parse_frames(raw: &[u8]) -> Vec<serde_json::Value> {
        String::from_utf8_lossy(raw)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str(l)
                    .unwrap_or_else(|e| panic!("campd put a NON-JSON line on the wire: {l:?}: {e}"))
            })
            .collect()
    }

    /// Pump until the subscriber is genuinely DRAINED — every byte framed AND
    /// every frame written. Dropping the `Conn` while `out` still holds bytes
    /// closes the socket on unsent data and TRUNCATES the last frame: a bug in the
    /// test, not in `pump`. (A concurrent reader must be draining the client end,
    /// or the socket stays full and this never completes.)
    fn pump_to_completion(rt: &mut ControlRuntime, token: Token, conn: &mut Conn) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            rt.pump(token, conn, t0());
            let s = rt.test_sub(token);
            let fs = s.file();
            if s.out.is_empty() && !fs.held && fs.cursor == fs.tail {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pump never drained: cursor={} tail={} out={} held={}",
                fs.cursor,
                fs.tail,
                s.out.out.len(),
                fs.held
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Read whatever the client end has RIGHT NOW (no pump is running concurrently).
    fn drain_client(client: &mut std::os::unix::net::UnixStream) -> Vec<serde_json::Value> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 65536];
        loop {
            match client.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break, // WouldBlock: nothing more right now
            }
        }
        parse_frames(&buf)
    }

    const T: Token = Token(6);

    /// C2: the worker's line is SPLICED IN VERBATIM — never re-serialized through
    /// a `Value`, which would SORT its keys and hand cp-2/cp-4 a wire camp
    /// invented by accident.
    #[test]
    fn event_frame_splices_verbatim_and_refuses_a_non_object_line() {
        // `type` before `subtype` is NOT alphabetical — a Value round-trip would
        // reorder them. This assertion is what makes the splice observable.
        let frame = event_frame("t/dev/1", 123, br#"{"type":"system","subtype":"init"}"#).unwrap();
        assert_eq!(
            String::from_utf8(frame).unwrap(),
            "{\"frame\":\"event\",\"session\":\"t/dev/1\",\"offset\":123,\
             \"event\":{\"type\":\"system\",\"subtype\":\"init\"}}\n"
        );

        // Not a JSON OBJECT => None; the caller emits skipped{not_a_json_object}.
        assert!(event_frame("t/dev/1", 1, b"not json").is_none());
        assert!(
            event_frame("t/dev/1", 1, b"[1,2,3]").is_none(),
            "an ARRAY is valid JSON but splicing it would emit an invalid frame — \
             deliberately STRICTER than cp-0, which counts any JSON value as parsed"
        );
        assert!(event_frame("t/dev/1", 1, b"42").is_none());
    }

    /// The whole frame wire, pinned. cp-2/3/4/5 all extend it.
    #[test]
    fn subscribe_frame_shapes_are_pinned() {
        assert_eq!(
            String::from_utf8(skipped_frame("t/dev/1", 456, 2_097_152, "over_cap")).unwrap(),
            "{\"frame\":\"skipped\",\"session\":\"t/dev/1\",\"offset\":456,\
             \"bytes\":2097152,\"reason\":\"over_cap\"}\n"
        );
        assert_eq!(
            String::from_utf8(skipped_frame("t/dev/1", 460, 17, "not_a_json_object")).unwrap(),
            "{\"frame\":\"skipped\",\"session\":\"t/dev/1\",\"offset\":460,\
             \"bytes\":17,\"reason\":\"not_a_json_object\"}\n"
        );
        assert_eq!(
            String::from_utf8(end_frame("t/dev/1", 789, "stopped")).unwrap(),
            "{\"frame\":\"end\",\"session\":\"t/dev/1\",\"offset\":789,\"reason\":\"stopped\"}\n"
        );
        // The hello carries the protocol version — the last free place for it.
        assert_eq!(
            serde_json::to_string(&Response::Subscribed {
                ok: true,
                v: 1,
                subscription: "sub-1".into(),
                cursor: 0,
            })
            .unwrap(),
            r#"{"ok":true,"v":1,"subscription":"sub-1","cursor":0}"#
        );
    }

    /// G1: a line LONGER THAN ONE CHUNK is buffered across chunks and delivered as
    /// ONE frame.
    ///
    /// With only a cursor and "lex each complete line in the chunk", a 100 KiB line
    /// contains no '\n' in its first 64 KiB chunk, advances nothing, and LIVELOCKS
    /// campd at 100% CPU. A Read/Bash/Grep tool-result line routinely exceeds
    /// 64 KiB — this is the ORDINARY case, not a pathological one.
    #[test]
    fn pump_lexes_a_line_that_spans_many_chunks() {
        let pad = "x".repeat(100 * 1024); // > HISTORY_CHUNK_BYTES
        let line = format!("{{\"type\":\"assistant\",\"text\":\"{pad}\"}}\n");
        let (_d, file, tail) = stream_file(line.as_bytes());

        let mut rt = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
        let (mut client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);
        // Reader thread: a real client drains as campd writes.
        let reader = std::thread::spawn(move || frames_to_eof(&mut client));
        pump_to_completion(&mut rt, T, &mut conn);
        drop(conn);
        let frames = reader.join().unwrap();

        assert_eq!(frames.len(), 1, "ONE event frame: {frames:?}");
        assert_eq!(frames[0]["frame"], "event");
        assert_eq!(frames[0]["event"]["text"], pad);
        assert_eq!(
            rt.test_sub(T).file().cursor,
            tail,
            "the cursor advanced exactly past the whole line"
        );
    }

    /// G1/C8: a line LARGER THAN THE CAP switches to OVERSIZE SCAN — counted, never
    /// buffered — and a `skipped{over_cap}` frame carries its TRUE byte count.
    ///
    /// The frame is only reachable BECAUSE the scan can lex a line it refuses to
    /// buffer. A pump that cannot lex an over-cap line makes this frame
    /// structurally unreachable.
    #[test]
    fn pump_skips_an_over_cap_line_without_buffering_it() {
        let cap = 8 * 1024;
        let pad = "x".repeat(64 * 1024); // way over the cap
        let monster = format!("{{\"type\":\"assistant\",\"text\":\"{pad}\"}}\n");
        let after = "{\"type\":\"assistant\",\"text\":\"after the monster\"}\n";
        let content = format!("{monster}{after}");
        let (_d, file, tail) = stream_file(content.as_bytes());

        let mut rt = ControlRuntime::new(cap);
        let (mut client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);
        let reader = std::thread::spawn(move || frames_to_eof(&mut client));
        // `partial` is NEVER allowed past the cap — that IS the RSS bound.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            rt.pump(T, &mut conn, t0());
            assert!(
                rt.test_sub(T).file().partial.len() <= cap,
                "partial grew past the cap: {}",
                rt.test_sub(T).file().partial.len()
            );
            let s = rt.test_sub(T);
            let fs = s.file();
            if s.out.is_empty() && !fs.held && fs.cursor == fs.tail {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "pump never drained");
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(conn);
        let frames = reader.join().unwrap();

        assert_eq!(frames[0]["frame"], "skipped");
        assert_eq!(frames[0]["reason"], "over_cap");
        assert_eq!(
            frames[0]["bytes"].as_u64().unwrap(),
            monster.len() as u64 - 1,
            "the TRUE byte count of the line (without its newline)"
        );
        // The cursor advanced PAST the monster, so the next line still arrives.
        assert_eq!(frames[1]["frame"], "event");
        assert_eq!(frames[1]["event"]["text"], "after the monster");
        assert_eq!(rt.test_sub(T).file().cursor, tail);
        assert!(
            rt.test_sub(T).file().oversize.is_none(),
            "the oversize scan ended at the newline"
        );
    }

    /// R1 — THE CAP IS A STOP, NOT A KILL.
    ///
    /// With a non-draining socket, FILL fills `out` to the cap and then STOPS: the
    /// complete line stays in `partial`, `held` is true, `cursor` does NOT advance,
    /// and NOTHING is dropped. Then the socket drains and the held line goes out as
    /// its own frame.
    ///
    /// A cap that KILLS drops every client that joins more than ~1 MiB behind the
    /// tail, however fast it reads — because during catch-up the producer is a FILE
    /// read and a file ALWAYS outruns a socket.
    #[test]
    fn a_frame_that_would_cross_the_cap_stalls_and_the_line_is_held() {
        let cap = 4 * 1024;
        let line = "{\"type\":\"assistant\",\"text\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}\n";
        // The SOCKET buffer must fill BEFORE `out` can ever reach the cap — and it is
        // wildly platform-dependent (~8 KiB on macOS, ~208 KiB on Linux). Size the
        // history past BOTH, then drive until a line is actually held: a fixed pump
        // count would silently prove nothing on the roomier kernel.
        let content = line.repeat(20_000);
        let (_d, file, tail) = stream_file(content.as_bytes());

        let mut rt = ControlRuntime::new(cap);
        // The client NEVER reads => the socket fills, then `out` fills to the cap.
        let (mut client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !rt.test_sub(T).file().held {
            match rt.pump(T, &mut conn, t0()) {
                PumpOutcome::Ok => {}
                PumpOutcome::Drop(_) => panic!(
                    "THE CAP IS A STOP, NOT A KILL — a subscriber that is merely BEHIND \
                     must never be dropped"
                ),
                PumpOutcome::Gone => panic!("the peer is still there"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no line was ever HELD — the cap was never reached, so this test proves \
                 nothing"
            );
        }
        let sub = rt.test_sub(T);
        let fs = sub.file();
        assert!(sub.out.out.len() <= cap, "out is bounded by the cap");
        assert!(fs.held, "the complete line is HELD, not lost");
        assert!(
            !fs.partial.is_empty(),
            "and it is still IN `partial` — nothing was thrown away"
        );
        let held_cursor = fs.cursor;
        assert!(
            held_cursor < tail,
            "the cursor did NOT advance past the line it could not send"
        );

        // Now the client reads. The held line goes out AS ITS OWN FRAME.
        let frames = drain_client(&mut client);
        assert!(!frames.is_empty());
        rt.pump(T, &mut conn, t0());
        assert!(
            rt.test_sub(T).file().cursor > held_cursor,
            "once the socket drained, the held line was delivered and the cursor moved"
        );
    }

    /// G3: `out` keeps FILLING while non-empty, across many chunks, up to the cap —
    /// which is what makes the cap meaningful at all. (Refilling only when EMPTY
    /// means `out` never holds more than one chunk, the cap is unreachable, and the
    /// drop path is DEAD CODE.)
    ///
    /// The DROP is not tested here: it fires at the STALL TIMEOUT, and its own test
    /// owns it.
    #[test]
    fn out_keeps_filling_while_non_empty_up_to_the_cap() {
        // The cap must EXCEED one chunk, or "accumulates across MANY chunks" is not
        // even expressible.
        let cap = 256 * 1024;
        let line = "{\"type\":\"assistant\",\"text\":\"filler\"}\n";
        let content = line.repeat(4000);
        let (_d, file, tail) = stream_file(content.as_bytes());

        let mut rt = ControlRuntime::new(cap);
        let (_client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);
        // The client never reads, so the socket fills and `out` accumulates.
        for _ in 0..8 {
            rt.pump(T, &mut conn, t0());
        }
        let out = rt.test_sub(T).out.out.len();
        assert!(
            out > HISTORY_CHUNK_BYTES,
            "`out` must accumulate across MANY chunks (it held {out} bytes) — \
             otherwise the cap is unreachable and the drop path is dead code"
        );
        assert!(out <= cap, "...and it STOPS at the cap: {out} > {cap}");
    }

    /// G2: a non-empty `out` must arm NOTHING. It means the last write returned
    /// WouldBlock, and the correct wakeup is the WRITABLE EDGE — already registered.
    ///
    /// Arming ZERO on top of it turns every blocked write into a SPIN: poll(0) ->
    /// pump -> WouldBlock -> poll(0)… And this is the COMMON case, not the
    /// pathological one — macOS's socket send buffer (~8 KiB) is far smaller than
    /// one chunk's worth of frames, so EVERY healthy subscriber WouldBlocks on
    /// essentially every chunk. campd would spin for the duration of any stream.
    #[test]
    fn poll_timeout_never_arms_on_a_wouldblock_alone() {
        let line = "{\"type\":\"assistant\",\"text\":\"x\"}\n";
        let content = line.repeat(4000);
        let (_d, file, tail) = stream_file(content.as_bytes());

        let mut rt = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
        let (_client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);

        // BEHIND with an EMPTY `out`: no fd will ever signal this — arm the
        // continuation.
        assert_eq!(
            rt.poll_timeout(t0()),
            Some(Duration::ZERO),
            "pumpable file work with an empty `out` must arm a continuation"
        );

        // Pump until the socket blocks and `out` is non-empty (the client reads
        // nothing).
        for _ in 0..6 {
            rt.pump(T, &mut conn, t0());
        }
        assert!(!rt.test_sub(T).out.is_empty(), "the write blocked");

        // THE ANTI-SPIN PROPERTY: with `out` NON-EMPTY, poll_timeout must NOT be
        // ZERO. The only thing armed is the STALL DEADLINE — a real deadline in the
        // future. The wakeup that matters here is the WRITABLE EDGE, and it is
        // already registered.
        let armed = rt.poll_timeout(t0()).expect("the stall deadline is armed");
        assert_ne!(
            armed,
            Duration::ZERO,
            "a blocked write must NEVER arm a zero timeout — that is a SPIN: \
             poll(0) -> pump -> WouldBlock -> poll(0)…, for the whole duration of \
             every stream"
        );
        assert_eq!(
            armed, SUBSCRIBER_STALL_TIMEOUT_DEFAULT,
            "what is armed is the stall deadline, and nothing else"
        );
    }

    /// V1 — `poll_timeout`'s `held` ARM, GATED AT LAST.
    ///
    /// This is one of the seven bugs six plan rounds fought over: *a held line
    /// stranded at `scan == tail` — never retried, no wakeup armed, no `end` frame.*
    /// The code was correct and **nothing tested it**: deleting `s.held ||` from
    /// `poll_timeout` passed the ENTIRE suite, unit and integration.
    ///
    /// The state is reachable: FILL cap-stops on the LAST line (`held`, `out` full),
    /// FLUSH then drains `out` completely, and the per-wake scan budget prevents the
    /// retry in the same call — leaving `held && out.is_empty() && scan == tail`,
    /// `blocked_since == None`, and NO pending WRITABLE edge. Without the `held`
    /// clause `poll_timeout` returns the 30 s stall deadline (or `None`), and **the
    /// last line of the history is never delivered.**
    #[test]
    fn poll_timeout_arms_for_a_line_held_at_the_tail() {
        let line = "{\"type\":\"assistant\",\"text\":\"x\"}\n";
        let (_d, file, tail) = stream_file(line.repeat(20_000).as_bytes());
        let mut rt = ControlRuntime::new(2048);
        let (_client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);

        // Drive to a REAL cap-stop (the client never reads).
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !rt.test_sub(T).file().held {
            rt.pump(T, &mut conn, t0());
            assert!(
                std::time::Instant::now() < deadline,
                "no line was ever held — this test proves nothing"
            );
        }

        // ...and now make that held line the LAST one, with `out` flushed: exactly
        // the terminal state of a catch-up that ran at the cap.
        let sub = rt.subscribers.get_mut(&T).expect("the subscriber");
        sub.out.out.clear();
        match &mut sub.source {
            Source::File(fs) => fs.tail = fs.scan,
            Source::Fleet(_) => unreachable!("this test inserts a file subscriber"),
        }

        assert_eq!(
            rt.poll_timeout(t0()),
            Some(Duration::ZERO),
            "a HELD line is real, pending work. Nothing else will EVER wake this \
             subscriber: `blocked_since` is None (the peer is reading), no WRITABLE \
             edge is pending once `out` drains, and `scan == tail` so there is no file \
             work. Without an armed continuation the last line of the history is never \
             delivered — and the end frame never follows it"
        );
    }

    /// §4.4: `MAX_SUBSCRIBERS` is the local-DoS bound — 8 idle connections must not
    /// be able to disable `subscribe` for everyone. It had NO test at all: the
    /// mutation `if false && …` survived the whole suite.
    #[test]
    fn the_subscriber_cap_refuses_the_ninth_and_a_detach_frees_the_slot() {
        let (_dir, mut ledger) = temp_ledger();
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("t-dev-1.json"), b"{\"type\":\"system\"}\n").unwrap();
        let mut rc = ReadChannelRuntime::new(sessions, 256 * 1024 * 1024).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        rc.drain_all(&mut ledger).unwrap();

        let mut rt = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
        for i in 0..MAX_SUBSCRIBERS {
            let r = rt.serve_subscribe(Token(100 + i), "t/dev/1", Some(0), &rc);
            assert!(
                matches!(r, Response::Subscribed { .. }),
                "subscriber {i} must be accepted: {r:?}"
            );
        }
        assert_eq!(rt.subscriber_count(), MAX_SUBSCRIBERS);

        // The NINTH is refused, LOUDLY and by name.
        let r = rt.serve_subscribe(Token(999), "t/dev/1", Some(0), &rc);
        match r {
            Response::Error { ok, error } => {
                assert!(!ok);
                assert!(
                    error.contains("MAX_SUBSCRIBERS"),
                    "the refusal must name the cap it hit: {error}"
                );
            }
            other => panic!(
                "the {}th subscription must be REFUSED — 8 idle connections must not \
                 be able to disable `subscribe` for everyone: {other:?}",
                MAX_SUBSCRIBERS + 1
            ),
        }

        // A DETACH frees the slot — the cap bounds concurrency, not lifetime totals.
        rt.forget(Token(100));
        assert_eq!(rt.subscriber_count(), MAX_SUBSCRIBERS - 1);
        assert!(matches!(
            rt.serve_subscribe(Token(1000), "t/dev/1", Some(0), &rc),
            Response::Subscribed { .. }
        ));
    }

    /// R3 — THE ~60-BYTE BAND. A line whose RAW length is under the cap but whose
    /// FRAME is not.
    ///
    /// Testing the LINE against the cap and the FRAME against the drop leaves this
    /// band: the line is never skipped, yet its frame cannot fit an empty `out`, so
    /// it takes the DROP path — a perfectly-reading subscriber killed,
    /// re-subscribing, re-reading the same line, dropped again, deterministically
    /// and FOREVER.
    #[test]
    fn a_line_whose_frame_just_exceeds_the_cap_is_skipped_not_dropped() {
        let cap = 4096;
        let overhead = measure_frame_overhead("t/dev/1");
        // Exactly ONE byte too long for its FRAME to fit the cap.
        let body_len = cap - overhead + 1;
        let filler = "x".repeat(body_len - 8); // the {"a":"…"} wrapper is 8 bytes
        let line = format!("{{\"a\":\"{filler}\"}}\n");
        assert_eq!(line.len() - 1, body_len, "the line is exactly at the band");
        assert!(line.len() - 1 < cap, "its RAW length is UNDER the cap");
        assert!(
            overhead + (line.len() - 1) > cap,
            "but its FRAME does not fit — this is the band"
        );

        let (_d, file, tail) = stream_file(line.as_bytes());
        let mut rt = ControlRuntime::new(cap);
        let (mut client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);
        let reader = std::thread::spawn(move || frames_to_eof(&mut client));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let outcome = rt.pump(T, &mut conn, t0());
            assert!(
                !matches!(outcome, PumpOutcome::Drop(_)),
                "a line in the frame-vs-line band must be SKIPPED, never DROP the \
                 subscriber — a dropped subscriber re-subscribes and is dropped again, \
                 forever"
            );
            let s = rt.test_sub(T);
            let fs = s.file();
            if s.out.is_empty() && !fs.held && fs.cursor == fs.tail {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "pump never drained");
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(conn);
        let frames = reader.join().unwrap();
        assert_eq!(frames[0]["frame"], "skipped");
        assert_eq!(frames[0]["reason"], "over_cap");
    }

    /// R3's SECOND HOLE: a line whose CROSSING BYTE IS THE '\n'.
    ///
    /// Pushing the byte BEFORE the cap test means the `continue` bypasses the
    /// newline check, `oversize` arms, and the scan runs on to the NEXT line's '\n'
    /// — SILENTLY CONSUMING A WHOLE LINE with no frame, and reporting a byte count
    /// that spans two. The newline must be tested FIRST.
    #[test]
    fn a_line_ending_exactly_at_the_cap_boundary_is_not_conflated_with_the_next() {
        let cap = 4096;
        let overhead = measure_frame_overhead("t/dev/1");
        // The LAST byte that still fits: pushing it leaves partial.len() + overhead
        // == cap. The '\n' is then the byte that would "cross".
        let body_len = cap - overhead;
        let filler = "x".repeat(body_len - 8);
        let first = format!("{{\"a\":\"{filler}\"}}\n");
        assert_eq!(first.len() - 1, body_len);
        let second = "{\"type\":\"assistant\",\"text\":\"the next line\"}\n";
        let content = format!("{first}{second}");
        let (_d, file, tail) = stream_file(content.as_bytes());

        let mut rt = ControlRuntime::new(cap);
        let (mut client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);
        let reader = std::thread::spawn(move || frames_to_eof(&mut client));
        pump_to_completion(&mut rt, T, &mut conn);
        drop(conn);
        let frames = reader.join().unwrap();

        assert_eq!(frames.len(), 2, "TWO lines, TWO frames: {frames:?}");
        // The first fits EXACTLY (frame.len() == cap) and is DELIVERED.
        assert_eq!(frames[0]["frame"], "event");
        // ...and the SECOND arrives as its OWN frame — not swallowed by an
        // oversize scan that ran past the first line's newline.
        assert_eq!(frames[1]["frame"], "event");
        assert_eq!(frames[1]["event"]["text"], "the next line");
    }

    /// R7/B4 — THE REFUSAL, and the U+FFFD that must NEVER reach the wire.
    ///
    /// JSON text is UTF-8 BY DEFINITION and `serde_json::from_slice` ENFORCES it, so
    /// a byte-identical round-trip of non-UTF-8 is unachievable by any
    /// implementation. The property actually worth having is that camp REFUSES the
    /// line rather than CORRUPTING it: a `&str` + `from_utf8_lossy` path would
    /// substitute U+FFFD and splice the corrupted bytes onto the wire, and no ASCII
    /// fixture would ever catch it.
    #[test]
    fn a_non_utf8_line_is_refused_not_silently_corrupted() {
        let mut content = Vec::new();
        content.extend_from_slice(br#"{"type":"assistant","text":""#);
        content.extend_from_slice(&[0xff, 0xfe, 0x80]); // raw non-UTF-8
        content.extend_from_slice(b"\"}\n");
        let (_d, file, tail) = stream_file(&content);

        // event_frame REFUSES it outright.
        assert!(
            event_frame("t/dev/1", 0, &content[..content.len() - 1]).is_none(),
            "from_slice ENFORCES UTF-8 — the line must be REFUSED"
        );

        let mut rt = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
        let (mut client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);
        let reader = std::thread::spawn(move || read_to_eof(&mut client));
        pump_to_completion(&mut rt, T, &mut conn);
        drop(conn);
        let raw = reader.join().unwrap();

        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.contains("not_a_json_object"),
            "the client must be told the line was SKIPPED: {text}"
        );
        // THE POINT: the corrupted bytes never reached the wire.
        assert!(
            !raw.windows(3).any(|w| w == [0xef, 0xbf, 0xbd]),
            "U+FFFD (the from_utf8_lossy replacement char) MUST NOT appear on the \
             wire — camp refuses the bytes, it does not rewrite them"
        );
    }

    /// R1's DROP RULE: the drop is a peer that has STOPPED READING — detected by
    /// ZERO bytes accepted across the stall timeout — never a large backlog.
    #[test]
    fn a_peer_that_accepts_nothing_is_dropped_at_the_stall_timeout() {
        let line = "{\"type\":\"assistant\",\"text\":\"x\"}\n";
        let content = line.repeat(20_000);
        let (_d, file, tail) = stream_file(content.as_bytes());

        let stall = Duration::from_millis(200);
        let mut rt = ControlRuntime::with_stall_timeout(64 * 1024, stall);
        // The client NEVER reads a single byte.
        let (_client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);

        // First pumps: the socket fills; `blocked_since` is STAMPED.
        for _ in 0..6 {
            assert!(matches!(rt.pump(T, &mut conn, t0()), PumpOutcome::Ok));
        }
        assert!(
            rt.test_sub(T).out.blocked_since.is_some(),
            "a zero-accept write must stamp `blocked_since`"
        );
        // The stall deadline is the ONLY thing that can ever fire for this
        // subscriber — nothing else will.
        assert!(rt.poll_timeout(t0()).is_some());

        // Past the stall timeout with STILL zero bytes accepted => DROPPED, loudly.
        let later = t0() + SignedDuration::from_millis(300);
        let event = match rt.pump(T, &mut conn, later) {
            PumpOutcome::Drop(e) => e,
            other => panic!(
                "a peer that has accepted ZERO bytes for the stall timeout must be \
                 dropped: {}",
                matches!(other, PumpOutcome::Ok)
            ),
        };
        assert_eq!(event.kind, EventType::SubscriberDropped);
        assert_eq!(data(&event)["session"], "t/dev/1");
        assert!(
            data(&event)["buffered_bytes"].as_u64().unwrap() > 0,
            "§4.4: the drop names the HIGH-WATER MARK"
        );
        assert_eq!(data(&event)["cap_bytes"], 64 * 1024);
    }

    /// THE CONVERSE, and it is the accepted residual: a client that accepts even
    /// ONE byte CLEARS `blocked_since` and is never dropped.
    ///
    /// A peer dripping one byte per interval can hold a buffer and a slot
    /// indefinitely. That is a DECISION — it IS reading, and any byte-rate floor is
    /// a policy number nobody has evidence for — recorded in the §4.4 amendment and
    /// as a cp-2 obligation, not an oversight.
    #[test]
    fn a_peer_that_accepts_even_one_byte_is_never_dropped() {
        let line = "{\"type\":\"assistant\",\"text\":\"x\"}\n";
        let content = line.repeat(20_000);
        let (_d, file, tail) = stream_file(content.as_bytes());

        let stall = Duration::from_millis(200);
        let mut rt = ControlRuntime::with_stall_timeout(64 * 1024, stall);
        let (mut client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);

        for _ in 0..6 {
            rt.pump(T, &mut conn, t0());
        }
        assert!(rt.test_sub(T).out.blocked_since.is_some());

        // The peer READS. It must drain enough that campd's NEXT write genuinely
        // accepts bytes — `blocked_since` is cleared by an accepted WRITE, not by the
        // peer's read, and the kernel's write granularity is not ours to assume
        // (freeing 4 KiB is enough on macOS; on Linux the next skb may still not fit,
        // so the write returns EAGAIN with ZERO bytes accepted and the peer looks
        // stalled — which, per §4.4, it correctly would be).
        let mut chunk = [0u8; 65536];
        let mut drained = 0usize;
        loop {
            match client.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => drained += n,
                Err(_) => break, // WouldBlock: the socket is empty
            }
        }
        assert!(drained > 0, "the test peer must actually accept some bytes");

        // WELL PAST the original stall deadline — but the peer accepted bytes, so
        // its stall clock RESTARTED and it is never dropped.
        let later = t0() + SignedDuration::from_millis(300);
        assert!(
            matches!(rt.pump(T, &mut conn, later), PumpOutcome::Ok),
            "a peer that is READING must never be dropped, however far behind it is"
        );
        // `blocked_since` may be freshly re-stamped (the socket filled again — it is
        // a slow reader, not a stopped one). What must NOT survive is the ORIGINAL
        // stamp: accepting a byte RESETS the clock.
        if let Some(since) = rt.test_sub(T).out.blocked_since {
            assert!(
                since >= later,
                "accepting ANY byte must RESET the stall clock — the original stamp \
                 must not survive, or a slow reader is eventually killed for being slow"
            );
        }
    }

    /// B3 — THE MECHANISM THAT NEVER FIRED.
    ///
    /// A `partial ends with '\n'` predicate is ALWAYS FALSE under the newline-first
    /// rule, so the held line is never retried: the NEXT line's bytes are appended
    /// onto it, TWO LINES ARE CONCATENATED into one body, `event_frame` rejects the
    /// result, and campd emits `skipped{not_a_json_object}` — corruption reported
    /// with a FALSE CAUSE. The flag must be REAL.
    #[test]
    fn a_line_held_at_the_cap_is_retried_and_never_concatenated() {
        let cap = 2048;
        // The SOCKET buffer (~8 KiB) must fill BEFORE `out` can ever reach the cap —
        // otherwise every frame is simply accepted and no line is ever HELD, and the
        // test would prove nothing.
        let a = "{\"type\":\"assistant\",\"n\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"}\n";
        let b = "{\"type\":\"assistant\",\"n\":\"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB\"}\n";
        let content = format!("{}{b}", a.repeat(1000));
        let (_d, file, tail) = stream_file(content.as_bytes());

        let mut rt = ControlRuntime::new(cap);
        // The client reads NOTHING yet: the socket fills, then `out` fills to the
        // cap, and then a complete line is HELD.
        let (mut client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !rt.test_sub(T).file().held {
            assert!(
                !matches!(rt.pump(T, &mut conn, t0()), PumpOutcome::Drop(_)),
                "THE CAP IS A STOP: a subscriber merely BEHIND is never dropped"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "a line was never HELD — the cap was never reached, so this test \
                 proves nothing"
            );
        }
        assert!(
            rt.test_sub(T).out.out.len() <= cap,
            "`out` is bounded by the cap"
        );
        assert!(
            !rt.test_sub(T).file().partial.is_empty(),
            "the held line is STILL IN `partial` — nothing was thrown away"
        );

        // Now the client drains, and the held line must be RETRIED as its OWN frame.
        let reader = std::thread::spawn(move || frames_to_eof(&mut client));
        pump_to_completion(&mut rt, T, &mut conn);
        drop(conn);
        let frames = reader.join().unwrap();

        // EVERY frame is a well-formed `event`. A `skipped{not_a_json_object}` here
        // would mean two lines were CONCATENATED into one body — corruption reported
        // with a FALSE CAUSE, which is exactly what a fake `held` flag produces.
        for f in &frames {
            assert_eq!(
                f["frame"], "event",
                "a held line must be retried AS ITS OWN FRAME — a `skipped` here means \
                 two lines were GLUED together: {f}"
            );
            assert_eq!(f["event"]["type"], "assistant");
        }
        assert_eq!(frames.len(), 1001, "every line, exactly once");
        assert_eq!(
            frames[1000]["event"]["n"],
            "B".repeat(36),
            "the line AFTER the held one is a SEPARATE frame, never glued to it"
        );
        assert_eq!(
            rt.test_sub(T).file().cursor,
            tail,
            "every byte was delivered"
        );
    }

    /// B2 — SILENT TRUNCATION. A stall MID-CHUNK must lose nothing.
    ///
    /// Advancing `scan` over the WHOLE chunk up front and then breaking mid-buffer
    /// throws away every byte after the stall point while `scan` already points past
    /// them: up to 64 KiB of lines SILENTLY LOST (§9: "never a silently truncated
    /// stream"), plus a permanent cursor/scan desync. `scan` must advance PER BYTE
    /// ABSORBED, so the remainder is simply re-read.
    #[test]
    fn a_cap_stop_mid_chunk_loses_no_bytes() {
        let cap = 2048; // far smaller than one chunk => FILL stalls mid-chunk
        let mut content = String::new();
        for i in 0..500 {
            content.push_str(&format!("{{\"type\":\"assistant\",\"i\":{i}}}\n"));
        }
        let (_d, file, tail) = stream_file(content.as_bytes());

        let mut rt = ControlRuntime::new(cap);
        let (mut client, mut conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);
        let reader = std::thread::spawn(move || frames_to_eof(&mut client));
        pump_to_completion(&mut rt, T, &mut conn);
        drop(conn);
        let frames = reader.join().unwrap();

        assert_eq!(
            frames.len(),
            500,
            "EVERY line in every chunk must arrive — a mid-chunk stall must leave \
             the remainder re-readable, not throw it away"
        );
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f["frame"], "event");
            assert_eq!(f["event"]["i"], i, "in order, none skipped");
        }
        assert_eq!(rt.test_sub(T).file().cursor, tail);
    }

    /// R5: the `end` frame reaches a CAUGHT-UP subscriber — driven deterministically,
    /// with no daemon, no notify and no timing.
    ///
    /// ⚠ THIS TEST DOES NOT PROVE THE DISPOSAL ORDERING, and no black-box test can:
    /// `close_disposed` is called here DIRECTLY with a hand-built `&[Disposed]`, so
    /// it cannot observe the event loop's call order at all. The ordering guarantee
    /// is STRUCTURAL — `take_disposed()` has exactly one caller, immediately after
    /// `dispose_pending()`, and `close_disposed` is not reachable from
    /// `control_step`. What this test proves is that the end frame ARRIVES. That is
    /// worth having, and it is all it is.
    #[test]
    fn close_disposed_emits_the_end_frame_for_a_caught_up_subscriber() {
        let (_dir, mut ledger) = temp_ledger();
        seed_live_session(&mut ledger, "t/dev/1");
        let line = "{\"type\":\"assistant\",\"text\":\"hi\"}\n";
        let (_d, file, tail) = stream_file(line.as_bytes());

        let mut rt = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
        let (mut client, conn) = rt.test_insert_subscriber(T, "t/dev/1", file, 0, tail);
        let mut conns: HashMap<Token, Conn> = HashMap::new();
        conns.insert(T, conn);

        // Caught up: everything delivered, `out` empty.
        rt.pump(T, conns.get_mut(&T).unwrap(), t0());
        let first = drain_client(&mut client);
        assert_eq!(first[0]["frame"], "event");

        // The session is reaped and disposed.
        let disposed = vec![Disposed {
            session: "t/dev/1".into(),
            final_offset: tail,
        }];
        let (gone, _events) = rt.close_disposed(&disposed, &ledger, &mut conns, t0());

        let frames = drain_client(&mut client);
        assert_eq!(frames.len(), 1, "the END frame: {frames:?}");
        assert_eq!(frames[0]["frame"], "end");
        assert_eq!(
            frames[0]["offset"].as_u64().unwrap(),
            tail,
            "the end frame's offset is the session's TRUE final offset"
        );
        // `stopped` or `crashed` — NEVER `capped`: that value does not exist in the
        // sessions table's status column.
        assert!(
            ["stopped", "crashed"].contains(&frames[0]["reason"].as_str().unwrap()),
            "reason was {:?}",
            frames[0]["reason"]
        );
        assert_eq!(gone, vec![T], "and the connection is closed (EOF follows)");
    }

    // ===== cp-2: the OutBuf seam + fleet source ============================

    #[test]
    fn outbuf_flush_stalls_a_peer_that_stops_reading_past_the_window() {
        use std::os::unix::net::UnixStream;
        let (client, server) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let mut conn = Conn {
            stream: mio::net::UnixStream::from_std(server),
            buf: Vec::new(),
        };
        // `client` is never read: the peer has stopped reading.

        let mut out = OutBuf::new();
        out.append(&vec![b'x'; 512 * 1024]); // more than one socket send buffer
        let t0 = jiff::Timestamp::now();
        let stall = std::time::Duration::from_millis(50);

        // Flush at t0 until the socket is full and the write WouldBlocks — THIS is
        // where blocked_since is stamped (a first Ok(n) partial write clears it, so
        // the stamp only survives once no more bytes are accepted).
        loop {
            match out.flush(&mut conn, t0, stall) {
                FlushStep::Drained => continue,
                FlushStep::WouldBlock => break,
                other => panic!("unexpected flush step before the window: {other:?}"),
            }
        }
        assert_eq!(
            out.blocked_since,
            Some(t0),
            "a zero-accept write stamps blocked_since at t0"
        );

        // One flush 60ms later — past the 50ms window — must Stall.
        let later = t0 + jiff::SignedDuration::from_millis(60);
        assert!(
            matches!(out.flush(&mut conn, later, stall), FlushStep::Stalled),
            "a peer that has not read for >= stall_timeout is Stalled"
        );
        drop(client);
    }

    #[test]
    fn one_pump_scans_at_most_the_per_wake_budget() {
        use std::io::Write as _;
        const T: Token = Token(9);
        // ~420 KiB of complete JSON lines: past MAX_PUMP_BYTES_PER_WAKE (256 KiB),
        // under the 1 MiB default cap — so the SCAN budget is the binding limit.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let mut f = std::fs::File::create(&path).unwrap();
        let line = format!(
            "{}\n",
            serde_json::json!({"type": "assistant", "pad": "x".repeat(60)})
        );
        let mut tail = 0u64;
        while tail < 420 * 1024 {
            f.write_all(line.as_bytes()).unwrap();
            tail += line.len() as u64;
        }
        f.flush().unwrap();

        let mut control = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT); // 1 MiB cap
        let file = std::fs::File::open(&path).unwrap();
        // Client NOT read: the socket fills and flush WouldBlocks — but the SCAN
        // budget still bounds FILL, so ONE pump cannot reach the tail.
        let (client, mut conn) = control.test_insert_subscriber(T, "t/dev/1", file, 0, tail);
        control.pump(T, &mut conn, t0());
        let scan = control.test_sub(T).file().scan;
        assert!(
            scan < tail,
            "one pump must NOT scan the whole history — the per-wake budget bounds it \
             (regression: scan={scan}, tail={tail})"
        );
        assert!(
            scan <= MAX_PUMP_BYTES_PER_WAKE as u64 + line.len() as u64,
            "one pump scans at most the budget + one line: scan={scan}"
        );
        drop(client);
    }

    /// Build (ledger, patrol, read_channel) with ONE campd-woken live session
    /// named "t/dev/1", agent "dev" — the minimal fixture for the model-building
    /// and hello tests. PatrolRuntime/ReadChannelRuntime are constructed as
    /// `daemon::run` does at startup, on a defaults-only config.
    fn fleet_fixture() -> (tempfile::TempDir, Ledger, PatrolRuntime, ReadChannelRuntime) {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        seed_live_session(&mut ledger, "t/dev/1");
        let config = camp_core::config::CampConfig::parse("[camp]\nname = \"t\"\n").unwrap();
        let patrol_config = camp_core::patrol::PatrolConfig::from_section(&config.patrol).unwrap();
        let patrol = PatrolRuntime::new(patrol_config, &config);
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let read_channel = ReadChannelRuntime::new(sessions, 256 * 1024 * 1024).unwrap();
        (dir, ledger, patrol, read_channel)
    }

    /// cp-2: the fleet model is `sessions.list`'s rows, reusable. A live session
    /// woken by campd appears as one `working` row addressed by name.
    #[test]
    fn fleet_model_returns_one_row_per_live_session() {
        let (_dir, ledger, patrol, read_channel) = fleet_fixture();
        let control = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
        let model = control
            .fleet_model(&ledger, &patrol, &read_channel)
            .unwrap();
        assert_eq!(model.len(), 1);
        assert_eq!(model[0].agent, "dev");
        assert_eq!(model[0].state, "working");
        assert!(
            !model[0].blocked,
            "cp-2 never sets blocked — cp-3 owns the producer"
        );
    }

    /// Drive one fleet subscriber to a quiet point against a fixed model. ONE
    /// `pump_with_model` call runs the whole FILL→FLUSH driver until the socket
    /// WouldBlocks or the delta is fully delivered, so a single call reaches the
    /// quiet point for the room-available sockets these tests use.
    fn pump_fleet_to_quiet(
        rt: &mut ControlRuntime,
        token: Token,
        conn: &mut Conn,
        model: &[SessionInfo],
    ) {
        match rt.pump_with_model(token, conn, jiff::Timestamp::now(), model) {
            PumpOutcome::Ok | PumpOutcome::Gone => {}
            PumpOutcome::Drop(_) => panic!("unexpected drop"),
        }
    }

    /// Read all currently-available newline JSON frames from a non-blocking client.
    fn read_frames(client: &std::os::unix::net::UnixStream) -> Vec<serde_json::Value> {
        use std::io::Read as _;
        let mut c = client.try_clone().unwrap();
        c.set_nonblocking(true).unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 8192];
        loop {
            match c.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&buf)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// cp-2 (§4.1): a fresh fleet subscriber gets the SNAPSHOT (one `session` frame
    /// per live row) then `synced`; a later state change pushes ONE delta frame; a
    /// departed session pushes a `gone` frame. Driven with no daemon, no timing.
    #[test]
    fn fleet_source_emits_snapshot_then_deltas_then_gone() {
        const T: Token = Token(1);
        let mut control = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
        let (client, mut conn) = control.test_insert_fleet_subscriber(T);

        let row = |name: &str, state: &str| SessionInfo {
            name: name.into(),
            agent: "dev".into(),
            rig: Some("gc".into()),
            bead: Some("gc-1".into()),
            state: state.into(),
            last_activity: "2026-07-14T00:00:00Z".into(),
            blocked: false,
        };

        let model = vec![row("camp/dev/1", "working"), row("camp/dev/2", "working")];
        pump_fleet_to_quiet(&mut control, T, &mut conn, &model);
        let frames = read_frames(&client);
        assert_eq!(frames.iter().filter(|f| f["frame"] == "session").count(), 2);
        assert!(
            frames.iter().any(|f| f["frame"] == "synced"),
            "snapshot ends with synced"
        );
        assert!(frames.iter().any(|f| f["frame"] == "session"
            && f["session"]["name"] == "camp/dev/1"
            && f["session"]["state"] == "working"));

        let model = vec![row("camp/dev/1", "stalled"), row("camp/dev/2", "working")];
        pump_fleet_to_quiet(&mut control, T, &mut conn, &model);
        let frames = read_frames(&client);
        assert_eq!(frames.len(), 1, "only the changed row is pushed");
        assert_eq!(frames[0]["frame"], "session");
        assert_eq!(frames[0]["session"]["name"], "camp/dev/1");
        assert_eq!(frames[0]["session"]["state"], "stalled");

        let model = vec![row("camp/dev/1", "stalled")];
        pump_fleet_to_quiet(&mut control, T, &mut conn, &model);
        let frames = read_frames(&client);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["frame"], "gone");
        assert_eq!(frames[0]["name"], "camp/dev/2");
        drop(client);
    }

    /// cp-2 (§4.2): the fleet frame shapes, byte-exact. A rename/reorder in
    /// FleetSessionFrame/FleetGoneFrame is a wire break and must turn this red.
    #[test]
    fn fleet_frame_shapes_are_pinned() {
        let s = SessionInfo {
            name: "camp/dev/1".into(),
            agent: "dev".into(),
            rig: Some("gc".into()),
            bead: Some("gc-1".into()),
            state: "working".into(),
            last_activity: "2026-07-14T00:00:00Z".into(),
            blocked: false,
        };
        assert_eq!(
            String::from_utf8(fleet_session_frame(&s)).unwrap(),
            "{\"frame\":\"session\",\"session\":{\"name\":\"camp/dev/1\",\"agent\":\"dev\",\
             \"rig\":\"gc\",\"bead\":\"gc-1\",\"state\":\"working\",\
             \"last_activity\":\"2026-07-14T00:00:00Z\",\"blocked\":false}}\n"
        );
        assert_eq!(
            String::from_utf8(fleet_gone_frame("camp/dev/1")).unwrap(),
            "{\"frame\":\"gone\",\"name\":\"camp/dev/1\"}\n"
        );
        assert_eq!(
            String::from_utf8(fleet_synced_frame()).unwrap(),
            "{\"frame\":\"synced\"}\n"
        );
    }

    /// cp-2 (§4.4): the cap is a STOP, not a Drop. With the client NOT reading, the
    /// socket fills and `flush` WouldBlocks; the ONLY thing keeping `out` bounded is
    /// the `has_room` STOP. Removing it lets `fill` append the whole (unbounded)
    /// model into `out`.
    #[test]
    fn fleet_cap_is_a_stop_and_out_never_exceeds_the_cap() {
        const T: Token = Token(1);
        let row = |i: usize| SessionInfo {
            name: format!("camp/dev/{i}"),
            agent: "dev".into(),
            rig: Some("gc".into()),
            bead: Some("gc-1".into()),
            state: "working".into(),
            last_activity: "2026-07-14T00:00:00Z".into(),
            blocked: false,
        };
        let frame_len = fleet_session_frame(&row(1)).len();
        let cap = frame_len * 2; // room for ~2 frames — far below the model's total
        let mut control = ControlRuntime::new(cap);
        let (client, mut conn) = control.test_insert_fleet_subscriber(T);
        // 300 rows (~60 KiB of frames) >> socket send buffer (~8 KiB) and >> cap, so
        // once the socket fills, `out` growth is bounded ONLY by the has_room STOP.
        let model: Vec<SessionInfo> = (0..300).map(row).collect();
        // DO NOT read the client. A few pumps, all within stall_timeout so no Drop.
        for _ in 0..4 {
            assert!(
                !matches!(
                    control.pump_with_model(T, &mut conn, jiff::Timestamp::now(), &model),
                    PumpOutcome::Drop(_)
                ),
                "the cap is a STOP, never a Drop"
            );
            assert!(
                control.test_sub(T).out.out.len() <= cap,
                "out must never exceed the cap while the socket is full — the cap is a STOP \
                 (out={}, cap={cap})",
                control.test_sub(T).out.out.len()
            );
        }
        drop(client);
    }

    /// cp-2 (§4.4): a stalled fleet subscriber is dropped LOUDLY, and the event
    /// names `"(fleet)"`, not a worker.
    #[test]
    fn a_stalled_fleet_subscriber_is_dropped_loudly_naming_fleet() {
        const T: Token = Token(1);
        let stall = std::time::Duration::from_millis(50);
        // cap small so `out` stays non-empty (blocked_since persists) while the
        // socket is full; stall short so the window is crossed deterministically.
        let mut control = ControlRuntime::with_stall_timeout(4096, stall);
        let (client, mut conn) = control.test_insert_fleet_subscriber(T);
        let row = |i: usize| SessionInfo {
            name: format!("camp/dev/{i}"),
            agent: "dev".into(),
            rig: Some("gc".into()),
            bead: Some("gc-1".into()),
            state: "working".into(),
            last_activity: "2026-07-14T00:00:00Z".into(),
            blocked: false,
        };
        // The model must exceed the socket's send+recv buffer so the first pump
        // WouldBlocks and stamps blocked_since. macOS's buffer is ~8 KiB, but
        // Linux's default is ~200 KiB PER DIRECTION — a 500-row model (~75 KiB)
        // drained clean there and the drop never fired (CI-only failure). 8000
        // rows (~1.2 MiB) overflow any realistic buffer.
        let model: Vec<SessionInfo> = (0..8000).map(row).collect();
        let t0 = jiff::Timestamp::now();
        // First pump: the driver loops FILL→FLUSH until WouldBlock, filling the
        // socket and stamping blocked_since=t0. Client NOT read.
        let _ = control.pump_with_model(T, &mut conn, t0, &model);
        assert!(
            control.test_sub(T).out.blocked_since.is_some(),
            "the socket must fill and stamp blocked_since at t0 — enlarge the model if this fails \
             on a runner with an even larger socket buffer"
        );
        // A pump 60ms later — past the 50ms window — drops the peer LOUDLY.
        let later = t0 + jiff::SignedDuration::from_millis(60);
        match control.pump_with_model(T, &mut conn, later, &model) {
            PumpOutcome::Drop(ev) => {
                assert_eq!(ev.kind, EventType::SubscriberDropped);
                assert_eq!(
                    ev.data["session"], "(fleet)",
                    "a fleet drop names (fleet), not a worker"
                );
                assert!(
                    ev.data["buffered_bytes"].as_u64().unwrap() > 0,
                    "names the high-water mark"
                );
            }
            _ => panic!("a fleet peer that stopped reading must be dropped loudly"),
        }
        drop(client);
    }

    /// cp-2 (§4.1): two subscribers at different points get different deltas from
    /// the SAME model — no `sent` leakage, no dropped update for the second.
    #[test]
    fn two_fleet_subscribers_diverge_by_their_own_sent_state() {
        const A: Token = Token(1);
        const B: Token = Token(2);
        let mut control = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
        let row = |name: &str| SessionInfo {
            name: name.into(),
            agent: "dev".into(),
            rig: Some("gc".into()),
            bead: Some("gc-1".into()),
            state: "working".into(),
            last_activity: "2026-07-14T00:00:00Z".into(),
            blocked: false,
        };

        // A catches up on a ONE-session model and drains its snapshot.
        let (ca, mut conna) = control.test_insert_fleet_subscriber(A);
        let m1 = vec![row("camp/dev/1")];
        pump_fleet_to_quiet(&mut control, A, &mut conna, &m1);
        let _ = read_frames(&ca);

        // B subscribes now; BOTH pumped against a TWO-session model.
        let (cb, mut connb) = control.test_insert_fleet_subscriber(B);
        let m2 = vec![row("camp/dev/1"), row("camp/dev/2")];
        pump_fleet_to_quiet(&mut control, A, &mut conna, &m2);
        pump_fleet_to_quiet(&mut control, B, &mut connb, &m2);

        let fa = read_frames(&ca);
        let fb = read_frames(&cb);
        // A already had dev/1 -> ONLY dev/2 is new (no re-send of dev/1).
        assert_eq!(fa.iter().filter(|f| f["frame"] == "session").count(), 1);
        assert_eq!(
            fa.iter().find(|f| f["frame"] == "session").unwrap()["session"]["name"],
            "camp/dev/2"
        );
        // B is fresh -> the FULL snapshot: both sessions + synced.
        assert_eq!(fb.iter().filter(|f| f["frame"] == "session").count(), 2);
        assert!(fb.iter().any(|f| f["frame"] == "synced"));
        drop(ca);
        drop(cb);
    }

    /// cp-2 (§4.1/§4.4): the hello registers a fleet subscriber and answers
    /// synchronously with `FleetSubscribed` (bounded by REQUEST_TIMEOUT, like every
    /// other verb). MAX_SUBSCRIBERS bounds it.
    #[test]
    fn serve_fleet_subscribe_registers_and_answers_the_hello() {
        let (_dir, ledger, patrol, read_channel) = fleet_fixture();
        let mut control = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
        let response = control.serve_fleet_subscribe(Token(7), &ledger, &patrol, &read_channel);
        assert!(matches!(
            response,
            Response::FleetSubscribed { ok: true, v: 1, .. }
        ));
        assert_eq!(
            control.subscriber_count(),
            1,
            "a fleet subscriber is registered"
        );
    }
}

// ===========================================================================
// cp-1 (§4.4, §9): session.subscribe — ONE monotone cursor, a Closing state,
// a skip policy.
//
// D6": a subscriber holds an open stream FILE, a single `cursor` (the next byte
// it needs) and `tail` (what campd has actually DRAINED). `pump` reads only
// [cursor, tail), frames each complete line, and advances the cursor. There is
// no catch-up/live distinction, hence no boundary to get wrong:
//   - truncation is impossible   — the cursor never skips a byte;
//   - duplication is impossible  — the cursor is monotone and is the sole gate;
//   - reading undrained bytes is impossible — reads are bounded by `tail`.
// A "live" line is just `tail` advancing.
// ===========================================================================

/// §4.4's number: the per-subscriber outbound buffer cap.
pub const SUBSCRIBER_BUFFER_BYTES: usize = SUBSCRIBER_BUFFER_BYTES_DEFAULT;

/// One file read per FILL pass.
pub const HISTORY_CHUNK_BYTES: usize = 64 * 1024;

/// G1: this bounds the SCAN, not merely the delivered bytes — otherwise an
/// over-cap line (which is scanned but never buffered) would be unbounded work
/// on the event loop. A 256 MiB line is consumed over many wakes, each doing
/// bounded work, and it TERMINATES.
pub const MAX_PUMP_BYTES_PER_WAKE: usize = 256 * 1024;

/// §4.4 bounds BYTES PER CONNECTION; nothing bounded the CONNECTION COUNT.
///
/// WORST CASE, STATED: each subscriber holds `out` <= cap AND `partial` <= cap,
/// so ~2 MiB each, ~16 MiB at 8 — on top of campd's idle RSS. That CAN approach
/// the spec's <20 MB figure, so plainly: **<20 MB is an IDLE bound** (and it is
/// what `make perf` measures — N subscribers with EMPTY buffers). A campd with 8
/// SATURATED subscribers is outside that bound BY DESIGN, and this cap is what
/// keeps it bounded at all. Raising it is a spec question, not a local call.
pub const MAX_SUBSCRIBERS: usize = 8;

/// R1: how long a peer may accept ZERO bytes, with data buffered for it, before
/// campd drops it.
///
/// THIS — not the size of `out` — is what a drop MEANS. A subscriber that is
/// merely BEHIND is not stalled; one whose socket has accepted nothing for 30 s
/// has stopped reading. The cap protects campd's memory against a peer that has
/// STOPPED READING; it must never be a verdict on a peer that is reading fast and
/// is simply behind, because during catch-up the producer is `pump` reading a
/// FILE and a file ALWAYS outruns a socket (macOS's unix-socket send buffer is
/// 8 KiB). Conflating the two drops every client that joins more than ~1 MiB
/// behind the tail, however fast it reads.
pub const SUBSCRIBER_STALL_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);

/// Test-only override, the `CAMP_SUBSCRIBER_BUFFER_BYTES` twin. WITHOUT IT the
/// stall tests are mandatory 30-second wall-clock tests, and their hard deadlines
/// would have to EXCEED 30 s — which makes a deadline useless as the hang
/// detector it exists to be. Fail fast on a malformed or zero value.
pub fn subscriber_stall_timeout_from_env(default: Duration) -> anyhow::Result<Duration> {
    match std::env::var("CAMP_SUBSCRIBER_STALL_TIMEOUT_MS") {
        Ok(raw) => {
            let ms: u64 = raw.parse().with_context(|| {
                format!("CAMP_SUBSCRIBER_STALL_TIMEOUT_MS={raw:?} is not a u64")
            })?;
            if ms == 0 {
                anyhow::bail!("CAMP_SUBSCRIBER_STALL_TIMEOUT_MS must be > 0");
            }
            Ok(Duration::from_millis(ms))
        }
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(v)) => {
            anyhow::bail!("CAMP_SUBSCRIBER_STALL_TIMEOUT_MS={v:?} is not valid UTF-8")
        }
    }
}

// ---- the frame wire — TAGGED FROM BIRTH ------------------------------------

#[derive(Serialize)]
struct FramePrefix<'a> {
    frame: &'static str,
    session: &'a str,
    offset: u64,
}

#[derive(Serialize)]
struct SkippedFrame<'a> {
    frame: &'static str,
    session: &'a str,
    offset: u64,
    bytes: u64,
    reason: &'static str,
}

#[derive(Serialize)]
struct EndFrame<'a> {
    frame: &'static str,
    session: &'a str,
    offset: u64,
    reason: &'a str,
}

/// C2/R7 — THE ONE SIGNATURE. `body` is BYTES, and `pump` never decodes UTF-8
/// anywhere.
///
/// **WHAT `offset` MEANS — PINNED, because cp-2 will get this wrong otherwise.**
/// A frame's `offset` is the byte offset of the **START OF THE NEXT LINE** — i.e.
/// just PAST the line this frame carries. It is a **RESUME CURSOR** (§9): hand it
/// back as `cursor` and you get the NEXT frame, byte-identical. It is **NOT** the
/// address of the event it carries, so `camp watch` cannot use it to say "this
/// event is at byte X" — it says "everything up to and including this event ends at
/// byte X". Those are different statements, and only the second one is true.
///
/// The worker's line is SPLICED IN VERBATIM — never round-tripped through a
/// `serde_json::Value`, which would SORT its keys (serde_json has no
/// `preserve_order`, and `raw_value` is a cargo feature this phase does not add).
/// A subscriber therefore sees EXACTLY the bytes the worker wrote.
///
/// The alternative — decoding to `&str` — invites `from_utf8_lossy`, which
/// SILENTLY REWRITES the worker's bytes (substituting U+FFFD): precisely the
/// corruption this byte-splice exists to prevent, and no ASCII fixture would
/// ever catch it.
///
/// Returns `None` when `body` is not a JSON OBJECT (splicing it would emit
/// invalid JSON); the caller emits `skipped{reason:"not_a_json_object"}`. NOTE
/// this is deliberately STRICTER than cp-0's parse, which accepts any JSON value
/// (a bare array or number counts as parsed there). That difference is honest,
/// not an agreement.
fn event_frame(session: &str, offset: u64, body: &[u8]) -> Option<Vec<u8>> {
    let body = trim_ascii_whitespace(body);
    if body.first() != Some(&b'{') {
        return None;
    }
    // from_SLICE, not from_str: no UTF-8 decode, ever. (JSON text IS UTF-8 by
    // definition and `from_slice` ENFORCES it — a line carrying raw non-UTF-8 is
    // therefore REFUSED here, and refusing is right. The `&str` +
    // `from_utf8_lossy` path would instead substitute U+FFFD and splice the
    // CORRUPTED bytes onto the wire.)
    if serde_json::from_slice::<serde_json::Value>(body).is_err() {
        return None;
    }
    let prefix = serde_json::to_string(&FramePrefix {
        frame: "event",
        session,
        offset,
    })
    .ok()?;
    // prefix == {"frame":"event","session":"…","offset":N} — replace its final
    // '}' with ,"event":<body>} so the raw bytes land untouched.
    let mut out = prefix.into_bytes();
    out.pop()?; // drop the closing '}'
    out.extend_from_slice(b",\"event\":");
    out.extend_from_slice(body); // VERBATIM
    out.extend_from_slice(b"}\n");
    Some(out)
}

fn skipped_frame(session: &str, offset: u64, bytes: u64, reason: &'static str) -> Vec<u8> {
    let mut line = serde_json::to_string(&SkippedFrame {
        frame: "skipped",
        session,
        offset,
        bytes,
        reason,
    })
    .unwrap_or_else(|_| String::from("{\"frame\":\"skipped\"}"));
    line.push('\n');
    line.into_bytes()
}

fn end_frame(session: &str, offset: u64, reason: &str) -> Vec<u8> {
    let mut line = serde_json::to_string(&EndFrame {
        frame: "end",
        session,
        offset,
        reason,
    })
    .unwrap_or_else(|_| String::from("{\"frame\":\"end\"}"));
    line.push('\n');
    line.into_bytes()
}

/// R3: the exact byte cost of an `event` frame's wrapper for THIS session,
/// MEASURED — never computed. At `u64::MAX`, the widest possible offset, so it
/// can never UNDER-estimate.
///
/// The over-cap decision is made on the FRAME, not the raw line. Testing the line
/// against the cap and the frame against the drop leaves a ~60-byte band in which
/// a perfectly-readable line is neither skipped nor deliverable — and its
/// subscriber is dropped, permanently, on every re-subscribe.
fn measure_frame_overhead(session: &str) -> usize {
    event_frame(session, u64::MAX, b"{}")
        .map(|f| f.len().saturating_sub(2))
        .unwrap_or(128)
}

fn trim_ascii_whitespace(mut b: &[u8]) -> &[u8] {
    while let Some((first, rest)) = b.split_first() {
        if first.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    while let Some((last, rest)) = b.split_last() {
        if last.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    b
}

/// What `pump` decided.
pub enum PumpOutcome {
    /// Nothing more to do RIGHT NOW. (`poll_timeout` owns any continuation.)
    Ok,
    /// The peer STOPPED READING — a durable `subscriber.dropped` (§4.4).
    Drop(EventInput),
    /// The peer is gone, or the `end` frame has flushed (C7).
    Gone,
}

/// One subscriber. Three reader positions, and the distinction is what makes a
/// long line survivable.
/// What one `flush` attempt did.
#[derive(Debug)]
pub enum FlushStep {
    /// The socket accepted bytes (out may still hold more) — the driver re-fills.
    Drained,
    /// The socket is full and the peer is still reading — the WRITABLE edge re-arms us.
    WouldBlock,
    /// R1: the peer accepted ZERO bytes for `stall_timeout` with data buffered — DROP it.
    Stalled,
    /// EPIPE / ECONNRESET / a zero-length write — the peer is gone.
    Gone,
}

/// The outbound half of every subscription — file OR fleet. It owns the §4.4
/// buffer-cap policy (a STOP), the R1 backpressure-stall policy (a DROP), and
/// the socket write. The stall rule is the ONLY drop policy, and it lives here
/// exactly once. "Hold the line in `partial`" is NOT here — that is a FILE
/// concept (a fleet row has no file), so it stays in `FileSource`.
pub struct OutBuf {
    /// Bytes queued for this socket. Bounded by the cap (a STOP, never a kill),
    /// plus at most one small over-cap `skipped` frame (see `FileSource`).
    pub out: Vec<u8>,
    /// The largest `out` reached — `buffered_bytes` in `subscriber.dropped`
    /// (§4.4: "naming the session and the high-water mark").
    ///
    /// NOTE: `out` is bounded by the cap *plus at most one small frame* — the
    /// `oversize` `skipped` frame is appended with no cap check (it is ~100 bytes and
    /// cannot be held, since the line it describes was never buffered). "out ≤ cap"
    /// is therefore off by one frame, deliberately, and stated.
    pub high_water: usize,
    /// R1: when the peer last accepted ZERO bytes with data buffered. Stamped on a
    /// zero-accept write, CLEARED the moment ANY byte is accepted.
    ///
    /// **RESIDUAL, RECORDED (round-2 review).** This clears only when campd's own
    /// `write()` accepts bytes — and a non-blocking write on a unix socket returns
    /// `EAGAIN` with ZERO bytes until free space reaches the kernel's LOW-WATER MARK
    /// (~2 KiB on macOS). So the code enforces *"a peer whose socket has not freed a
    /// low-water mark's worth"*, which is very slightly stronger than §4.4's *"a peer
    /// that stopped reading"*: a peer sipping < ~4 KiB per `SUBSCRIBER_STALL_TIMEOUT`
    /// (30 s at the shipped default) is dropped while still, technically, reading.
    /// At that rate it is indistinguishable from a stopped peer, so the reach is
    /// small — but it is a real difference between the spec's words and the code's,
    /// and it is written down rather than discovered later.
    pub blocked_since: Option<Timestamp>,
}

impl OutBuf {
    pub fn new() -> OutBuf {
        OutBuf {
            out: Vec::new(),
            high_water: 0,
            blocked_since: None,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }
    /// §4.4: does one more frame fit under the cap? The cap is a STOP — a source
    /// whose next frame does not fit HOLDS it (file: in `partial`; fleet: by not
    /// advancing `sent`) rather than dropping the peer.
    pub fn has_room(&self, frame_len: usize, cap: usize) -> bool {
        self.out.len() + frame_len <= cap
    }
    pub fn append(&mut self, frame: &[u8]) {
        self.out.extend_from_slice(frame);
        self.high_water = self.high_water.max(self.out.len());
    }
    /// ONE write attempt. cp-1's FLUSH block (C), verbatim minus the drop-event
    /// construction (the caller owns the event shape).
    pub fn flush(&mut self, conn: &mut Conn, now: Timestamp, stall_timeout: Duration) -> FlushStep {
        use std::io::Write as _;
        match conn.stream.write(&self.out) {
            Ok(0) => FlushStep::Gone,
            Ok(n) => {
                self.out.drain(..n);
                self.blocked_since = None; // R1: it IS reading.
                FlushStep::Drained
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => FlushStep::Drained,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // G2: the WRITABLE EDGE re-arms us. Do NOT arm a timeout here — a
                // zero timeout on a blocked write turns it into a SPIN, and since
                // macOS's socket buffer (~8 KiB) is far smaller than a chunk's worth
                // of frames, EVERY healthy subscriber hits this on essentially every
                // chunk.
                //
                // R1: but a peer that accepts ZERO bytes is a peer that has STOPPED
                // READING — and THAT, not the size of `out`, is what a drop means.
                if self.blocked_since.is_none() {
                    self.blocked_since = Some(now);
                }
                if let Some(since) = self.blocked_since
                    && now.duration_since(since) >= signed(stall_timeout)
                {
                    self.high_water = self.high_water.max(self.out.len());
                    return FlushStep::Stalled;
                }
                FlushStep::WouldBlock
            }
            // EPIPE / ECONNRESET: the peer is gone. A normal detach is NOT a fault
            // and appends NO event (§5.2).
            Err(_) => FlushStep::Gone,
        }
    }
}

/// The source half of a subscription. Task 1 ships only `File`; Task 4 adds
/// `Fleet`. The `OutBuf` is source-agnostic; the SOURCE is what differs.
pub enum Source {
    File(FileSource),
    Fleet(FleetSource),
}

/// cp-1's byte-cursor tailer, unchanged. One worker's stdout FILE, delivered
/// `[cursor, tail)` as `event`/`skipped`/`end` frames.
pub struct FileSource {
    pub session: String,
    /// The open stream file. Held across disposal ON PURPOSE — on Unix an
    /// unlinked inode survives while an fd is open, so a Closing subscriber
    /// FINISHES ITS HISTORY (C7).
    file: std::fs::File,

    /// THE DELIVERY CURSOR (D6"): the byte offset just past the last COMPLETE
    /// line delivered. MONOTONE, and the sole delivery gate — this is what a
    /// client RESUMES FROM (§9), so it may only ever advance past a whole line.
    cursor: u64,
    /// THE READ POSITION: how far `pump` has read. `scan >= cursor` always; the
    /// gap is exactly the in-progress line.
    ///
    /// G1: with only a cursor, a line longer than one 64 KiB chunk contains no
    /// '\n', advances nothing, and LIVELOCKS campd at 100% CPU. A Read/Bash/Grep
    /// tool-result line routinely exceeds 64 KiB.
    scan: u64,
    /// The bytes of the in-progress line, [cursor, scan). BOUNDED BY THE CAP.
    ///
    /// B1: when a line COMPLETES its '\n' IS PUSHED HERE before `try_emit_line`
    /// is called — because `off = cursor + partial.len()` must land PAST the
    /// newline. Omitting it makes `cursor` land ON the newline and drift ONE BYTE
    /// PER LINE, cumulatively — and §9 makes these offsets the durable RESUME
    /// CURSORS, so a client reconnecting with a cursor campd handed it lands
    /// mid-file at the wrong byte. A drifting offset still INCREASES, which is why
    /// no "offsets are strictly increasing" assertion can ever catch it.
    partial: Vec<u8>,
    /// B3: `partial` holds a COMPLETE line (terminated by its '\n') that did not
    /// fit `out`. A REAL FLAG — inspecting `partial` for a trailing '\n' is not a
    /// substitute, because the newline-first rule makes that test ALWAYS FALSE:
    /// the held line would never be retried, the next line's bytes would be
    /// appended onto it, and TWO LINES WOULD BE CONCATENATED into one body —
    /// rejected by `event_frame` and reported as `skipped{not_a_json_object}`:
    /// corruption with a FALSE CAUSE.
    ///
    /// `try_emit_line` is the ONLY writer and clears it on EVERY success path
    /// (including the blank-line path).
    held: bool,
    /// OVERSIZE SCAN (G1/C8): the in-progress line's FRAME cannot fit the whole
    /// cap. `partial` is DROPPED (memory freed) and bytes are merely COUNTED while
    /// scanning for the terminating '\n'. At the newline a
    /// `skipped{reason:"over_cap"}` frame carries the TRUE byte count — which is
    /// why the frame can carry a count at all. A `skipped` frame for a line that
    /// could never be LEXED would be structurally unreachable.
    oversize: Option<u64>,

    /// What campd has actually DRAINED. Refreshed every wake from
    /// `read_channel.tail_state`; PINNED to the final offset at disposal. `pump`
    /// reads ONLY [scan, tail), so it can never hand out bytes campd has not
    /// consumed.
    tail: u64,
    /// C7: set at disposal (`stopped` | `crashed`). A Closing subscriber keeps
    /// pumping until `scan == tail` AND `out` is empty; only THEN does the `end`
    /// frame go out, and the connection closes when that flush completes.
    closing: Option<String>,
    /// R2: the `end` frame has been APPENDED. Without this the TERMINAL branch
    /// re-fires on every loop iteration — appending an UNBOUNDED stream of
    /// duplicate `end` frames, never reaching the `out.is_empty()` test that is the
    /// ONLY path to `Gone`, so EOF never arrives and the fd and one of 8 subscriber
    /// slots are never released.
    end_sent: bool,
    /// R3: the measured byte cost of an `event` frame's wrapper for this session.
    frame_overhead: usize,
}

/// A subscription: an id, its outbound buffer (`OutBuf`, source-agnostic), and
/// its SOURCE (a file tailer, or — from Task 4 — the fleet model).
pub struct Subscriber {
    id: String,
    out: OutBuf,
    source: Source,
}

/// Test-only: reach the file source, so cp-1's subscriber tests keep reading
/// `.held`/`.cursor`/`.scan`/`.tail` (now nested under `Source::File`).
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
impl Subscriber {
    fn file(&self) -> &FileSource {
        match &self.source {
            Source::File(fs) => fs,
            Source::Fleet(_) => panic!("test_sub used on a non-file subscriber"),
        }
    }
}

impl FileSource {
    /// cp-1 blocks (A) FILL + (B) TERMINAL, verbatim. Emits frames INTO `out`
    /// via `out.has_room` / `out.append`, STOPPING at the cap (R1). `scanned` is
    /// the driver's per-pump-call budget: the FILL while-guard keeps
    /// `*scanned < MAX_PUMP_BYTES_PER_WAKE` and `*scanned += 1` runs per byte
    /// absorbed, EXACTLY as cp-1 did with its function-local `scanned`. Returns
    /// whether it is TERMINAL — the `end` frame appended (nothing left to give),
    /// OR a hard file inconsistency, both of which the driver turns into `Gone`.
    fn fill(
        &mut self,
        out: &mut OutBuf,
        cap: usize,
        scanned: &mut usize,
        pending_events: &mut Vec<EventInput>,
        degraded_seen: &mut HashSet<(String, u64)>,
    ) -> bool {
        use std::os::unix::fs::FileExt as _;

        // B3(e): RESET every fill call (= every outer driver iteration). Declared
        // here and never reset, "the socket took bytes, so FILL may resume" is
        // simply false.
        let mut stalled = false;

        // ---- (A) FILL: turn file bytes into frames, STOPPING at the cap -------
        //
        // R1: the cap is a STOP, not a kill.
        // B3(b): the guard admits a HELD line even at `scan == tail` — the normal
        // terminal state of any catch-up that ran at the cap. Gating FILL on
        // `scan < tail` alone strands such a line: nothing is armed to wake it, the
        // last line of the history is never delivered, and TERMINAL (which requires
        // an empty `partial`) can never fire — no end frame, no EOF, fd + slot leaked.
        while !stalled && (self.held || self.scan < self.tail) && *scanned < MAX_PUMP_BYTES_PER_WAKE
        {
            // B3(a): a COMPLETE line held over because `out` was full. (No
            // `stalled` flag needed here: this `break` leaves the FILL loop
            // directly. The flag exists for the BYTE loop below, whose `break`
            // only exits the inner `for` — the `while` guard then re-reads it and
            // stops FILL from pulling another chunk on top of a held line.)
            if self.held {
                if !self.try_emit_line(out, cap) {
                    break;
                }
                continue; // try_emit_line cleared `held`
            }

            let want = std::cmp::min(HISTORY_CHUNK_BYTES as u64, self.tail - self.scan) as usize;
            let mut buf = vec![0u8; want];
            let n = match self.file.read_at(&mut buf, self.scan) {
                // R4: the stream file is append-only; it CANNOT shrink. `scan <
                // tail` with a zero-byte read is a genuine inconsistency, not a
                // benign EOF — and left unhandled it advances neither `scan` nor
                // `out` while the loop guard stays true: campd HANGS inside pump.
                // The original returned PumpOutcome::Gone here (discarding `out`);
                // in the bool-return world that is: report LOUDLY once, DISCARD
                // `out` so the driver sees empty and returns Gone, terminal = true.
                Ok(0) => {
                    pending_events.push(degraded_event(
                        &self.session,
                        format!(
                            "subscribe: the stream file is SHORTER than campd's own drained \
                             offset (read 0 bytes at {} with tail {}). The file is append-only \
                             and cannot shrink, so this is a real inconsistency — the \
                             subscription is closed rather than looping forever",
                            self.scan, self.tail
                        ),
                    ));
                    out.out.clear();
                    return true;
                }
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    pending_events.push(degraded_event(
                        &self.session,
                        format!("subscribe: reading the stream file at {}: {e}", self.scan),
                    ));
                    out.out.clear();
                    return true;
                }
            };

            for &b in &buf[..n] {
                // B2: `scan` and `scanned` advance PER BYTE ABSORBED — never per
                // chunk read. Advancing over the whole chunk up front and then
                // breaking mid-buffer on a stall THROWS AWAY every byte after the
                // stall point while `scan` already points past it: up to 64 KiB of
                // SILENT LINE LOSS (§9: "never a silently truncated stream"), plus
                // a permanent cursor/scan desync. With per-byte accounting a stall
                // simply leaves the remainder at [scan, ..) and the next FILL
                // re-reads it. Nothing is lost.
                self.scan += 1;
                *scanned += 1;

                // ---- oversize: the line's FRAME cannot fit the whole cap.
                //      COUNT, never buffer.
                if let Some(count) = self.oversize {
                    if b == b'\n' {
                        let off = self.cursor + count + 1; // + the newline
                        out.append(&skipped_frame(&self.session, off, count, "over_cap"));
                        // G11: ONE durable event, deduped per (session, offset) —
                        // N subscribers hit the SAME over-cap line and must not
                        // append N events. (This dedupe is the whole reason the set
                        // exists.)
                        if degraded_seen.insert((self.session.clone(), self.cursor)) {
                            pending_events.push(degraded_event(
                                &self.session,
                                format!(
                                    "subscribe: a stream line of {count} bytes at offset {} \
                                     exceeds the subscriber buffer cap ({cap} bytes) and was \
                                     SKIPPED — subscribers received a skipped frame naming it. \
                                     campd itself never buffered it (§4.4)",
                                    self.cursor
                                ),
                            ));
                        }
                        self.cursor = off;
                        self.oversize = None;
                    } else {
                        self.oversize = Some(count + 1);
                    }
                    continue;
                }

                // ---- R3: THE NEWLINE IS TESTED FIRST, before any push or cap
                //      check. Pushing first and testing after means that when the
                //      CROSSING byte IS the '\n', the cap `continue` bypasses the
                //      newline check, `oversize` arms, and the scan runs on to the
                //      NEXT line's '\n' — silently consuming a whole line with no
                //      frame, and reporting a byte count spanning two.
                if b == b'\n' {
                    self.partial.push(b'\n'); // B1: THE NEWLINE GOES IN.
                    self.held = true;
                    if !self.try_emit_line(out, cap) {
                        stalled = true; // R1: HOLD the line. Never Drop.
                        break;
                    }
                    continue;
                }

                // ---- R3: the over-cap decision is made on the FRAME, not the raw
                //      line. Otherwise a line whose raw length is under the cap but
                //      whose FRAME is not can be neither skipped nor delivered, and
                //      its subscriber is dropped — deterministically, on every
                //      re-subscribe.
                if self.partial.len() + 1 + self.frame_overhead > cap {
                    self.oversize = Some(self.partial.len() as u64 + 1);
                    self.partial.clear(); // free the memory
                    continue;
                }

                self.partial.push(b);
            }
        }

        // ---- (B) TERMINAL: the full history FIRST, then the end frame, ONCE ----
        //
        // B3(d): `!self.held` — a HELD line is unfinished history, and a
        // `partial.is_empty()` test alone is satisfied while a held line waits.
        if !self.end_sent
            && out.is_empty()
            && self.closing.is_some()
            && self.scan == self.tail
            && !self.held
            && self.partial.is_empty()
            && self.oversize.is_none()
        {
            let reason = self.closing.clone().unwrap_or_else(|| "stopped".into());
            out.append(&end_frame(&self.session, self.tail, &reason));
            // R2: WITHOUT THIS, (B) re-fires on every iteration — unbounded
            // duplicate end frames, and `Gone` is never reached.
            self.end_sent = true;
        }

        self.end_sent
    }

    /// R1/R3: emit ONE complete line from `partial`, or report that `out` has no room
    /// for it.
    ///
    /// Returns `false` => the caller STALLS: `partial` KEEPS the complete line, `held`
    /// stays true, `cursor` does NOT advance, and NOTHING IS LOST. The cap is a STOP.
    ///
    /// PRECONDITION (established by the only caller): `self.held` is true and
    /// `self.partial` ENDS WITH '\n'.
    ///
    /// It can never stall FOREVER: the pre-push guard bounds
    /// `partial.len() + frame_overhead <= cap` BEFORE the '\n' goes in, and `body`
    /// strips that '\n' again — so `frame.len() = frame_overhead + body.len() <= cap`
    /// and this frame ALWAYS fits an EMPTY `out`. A held line goes out the moment the
    /// socket drains what is ahead of it.
    fn try_emit_line(&mut self, out: &mut OutBuf, cap: usize) -> bool {
        // B1: `partial` INCLUDES the '\n', so `off` lands PAST the newline — which is
        // what makes it a valid §9 resume cursor (the start of the NEXT line).
        let off = self.cursor + self.partial.len() as u64;

        let frame = {
            let mut end = self.partial.len().saturating_sub(1); // strip '\n'
            if end > 0 && self.partial[end - 1] == b'\r' {
                end -= 1;
            }
            let body = &self.partial[..end];
            if trim_ascii_whitespace(body).is_empty() {
                // G11: a blank line is a SILENT no-op — no frame, no event — exactly
                // as cp-0 treats it. Emitting a `skipped` frame for a no-op would be
                // noise a client has to filter.
                self.cursor = off;
                self.partial.clear();
                self.held = false;
                return true;
            }
            match event_frame(&self.session, off, body) {
                Some(f) => f,
                None => skipped_frame(&self.session, off, body.len() as u64, "not_a_json_object"),
            }
        };

        if !out.has_room(frame.len(), cap) {
            return false; // STALL (R1) — never a Drop.
        }

        out.append(&frame);
        self.cursor = off;
        self.partial.clear();
        self.held = false;
        true
    }
}

#[derive(Serialize)]
struct FleetSessionFrame<'a> {
    frame: &'static str,
    session: &'a SessionInfo,
}
#[derive(Serialize)]
struct FleetGoneFrame<'a> {
    frame: &'static str,
    name: &'a str,
}

fn fleet_session_frame(s: &SessionInfo) -> Vec<u8> {
    let mut line = serde_json::to_string(&FleetSessionFrame {
        frame: "session",
        session: s,
    })
    .unwrap_or_else(|_| String::from("{\"frame\":\"session\"}"));
    line.push('\n');
    line.into_bytes()
}
fn fleet_gone_frame(name: &str) -> Vec<u8> {
    let mut line = serde_json::to_string(&FleetGoneFrame {
        frame: "gone",
        name,
    })
    .unwrap_or_else(|_| String::from("{\"frame\":\"gone\"}"));
    line.push('\n');
    line.into_bytes()
}
fn fleet_synced_frame() -> Vec<u8> {
    b"{\"frame\":\"synced\"}\n".to_vec()
}

/// The LEDGER/model-sourced half of a subscription. It holds no file — its
/// "cursor" is the by-name snapshot `sent` it last delivered, which is why the
/// §4.4 cap-STOP here is "leave `sent` unchanged for an un-emitted row" (the
/// model is recomputable next wake) rather than "hold the line in `partial`".
pub struct FleetSource {
    sent: HashMap<String, SessionInfo>,
    synced: bool,
}

impl FleetSource {
    fn new() -> FleetSource {
        FleetSource {
            sent: HashMap::new(),
            synced: false,
        }
    }

    /// Diff `model` against `sent`, emitting the delta into `out` and STOPPING at
    /// the cap. NON-TERMINAL always — a fleet subscription only ends on client
    /// detach or campd shutdown.
    fn fill(
        &mut self,
        out: &mut OutBuf,
        cap: usize,
        model: &[SessionInfo],
        pending_events: &mut Vec<EventInput>,
    ) {
        // (1) added / changed rows.
        for s in model {
            if self.sent.get(&s.name) == Some(s) {
                continue;
            }
            let frame = fleet_session_frame(s);
            // Fail LOUD, never silent-livelock: a single frame that cannot fit an
            // EMPTY cap would stall forever (invariant 5). Unreachable in practice
            // (a SessionInfo frame is < 1 KiB, cap default 1 MiB) — HANDLED, not
            // assumed: report it and advance `sent` so the snapshot completes.
            if frame.len() > cap {
                pending_events.push(degraded_event(
                    &s.name,
                    format!(
                        "fleet: a session frame of {} bytes exceeds the subscriber buffer cap \
                         ({cap} bytes) and was SKIPPED for subscriber delivery",
                        frame.len()
                    ),
                ));
                self.sent.insert(s.name.clone(), s.clone());
                continue;
            }
            if !out.has_room(frame.len(), cap) {
                return; // R1 cap-STOP: `sent` unchanged; resumed next fill.
            }
            out.append(&frame);
            self.sent.insert(s.name.clone(), s.clone());
        }
        // (2) departed rows: in `sent` but not in `model`.
        let live: HashSet<&str> = model.iter().map(|s| s.name.as_str()).collect();
        let goners: Vec<String> = self
            .sent
            .keys()
            .filter(|n| !live.contains(n.as_str()))
            .cloned()
            .collect();
        for name in goners {
            let frame = fleet_gone_frame(&name);
            if !out.has_room(frame.len(), cap) {
                return;
            }
            out.append(&frame);
            self.sent.remove(&name);
        }
        // (3) the snapshot terminator, once.
        if !self.synced {
            let frame = fleet_synced_frame();
            if !out.has_room(frame.len(), cap) {
                return;
            }
            out.append(&frame);
            self.synced = true;
        }
    }
}

fn degraded_event(session: &str, error: String) -> EventInput {
    EventInput {
        kind: EventType::PatrolDegraded,
        rig: None,
        actor: "campd".into(),
        bead: None,
        data: serde_json::json!({ "session": session, "error": error }),
    }
}

/// R1/§4.4: the loud drop event. `session` names the source; a fleet drop uses
/// the marker `"(fleet)"` (Task 4). `subscription` + `buffered_bytes` +
/// `cap_bytes` are the high-water forensics §4.4 requires.
fn subscriber_dropped_event(sub: &Subscriber, cap: usize) -> EventInput {
    let session = match &sub.source {
        Source::File(fs) => fs.session.clone(),
        Source::Fleet(_) => "(fleet)".to_owned(),
    };
    EventInput {
        kind: EventType::SubscriberDropped,
        rig: None,
        actor: "campd".into(),
        bead: None,
        data: serde_json::json!({
            "session": session,
            "subscription": sub.id,
            "buffered_bytes": sub.out.high_water as u64,
            "cap_bytes": cap as u64,
        }),
    }
}

/// THE ONE DATA PATH (D6"), and the only place bytes reach a subscriber's socket.
///
/// The DRIVER: it owns the per-CALL scan budget (`scanned`), ties source FILL to
/// `OutBuf` FLUSH, and maps a stall to the loud drop. A free function, not a
/// method, because it needs `&mut Subscriber` and `&mut ControlRuntime`'s
/// collectors at the same time — disjoint fields, which the borrow checker only
/// accepts when they are named separately.
#[allow(clippy::too_many_arguments)]
fn pump_subscriber(
    sub: &mut Subscriber,
    conn: &mut Conn,
    now: Timestamp,
    cap: usize,
    stall_timeout: Duration,
    pending_events: &mut Vec<EventInput>,
    degraded_seen: &mut HashSet<(String, u64)>,
    fleet_model: &[SessionInfo], // the Fleet arm diffs against it; File ignores it
) -> PumpOutcome {
    // G1: the per-CALL scan budget. Reset ONCE here, PERSISTS across every
    // FILL→FLUSH→re-FILL below — this is what bounds work per wake. Making it
    // local to `fill` would reset it per re-fill and break the bound.
    let mut scanned = 0usize;
    loop {
        // FILL (source-specific), then FLUSH (OutBuf). The driver ties them.
        let terminal = match &mut sub.source {
            Source::File(fs) => fs.fill(
                &mut sub.out,
                cap,
                &mut scanned,
                pending_events,
                degraded_seen,
            ),
            Source::Fleet(fleet) => {
                fleet.fill(&mut sub.out, cap, fleet_model, pending_events);
                false // never terminal
            }
        };
        if sub.out.is_empty() {
            // Nothing to write. A TERMINAL file source with an empty out has
            // flushed its `end` frame (or hit a hard inconsistency) — it is Gone.
            // Otherwise it simply waits for the next wake (a live line, or a fleet
            // state change).
            return if terminal {
                PumpOutcome::Gone
            } else {
                PumpOutcome::Ok
            };
        }
        match sub.out.flush(conn, now, stall_timeout) {
            FlushStep::Drained => continue, // room freed — re-fill (scanned persists)
            FlushStep::WouldBlock => return PumpOutcome::Ok, // WRITABLE edge re-arms
            FlushStep::Gone => return PumpOutcome::Gone,
            FlushStep::Stalled => return PumpOutcome::Drop(subscriber_dropped_event(sub, cap)),
        }
    }
}

impl ControlRuntime {
    /// §4.4/§9: open a subscription.
    ///
    /// It REGISTERS; it never WRITES. The hello must be the FIRST bytes on the
    /// socket, and `respond()` uses `write_all` on a NON-BLOCKING stream — a
    /// WouldBlock there drops the connection. The caller writes the hello, then
    /// pumps.
    pub fn serve_subscribe(
        &mut self,
        token: Token,
        session: &str,
        cursor: Option<u64>,
        read_channel: &ReadChannelRuntime,
    ) -> Response {
        if self.subscribers.len() >= MAX_SUBSCRIBERS {
            return Response::Error {
                ok: false,
                error: format!(
                    "campd already has {MAX_SUBSCRIBERS} subscriptions open (the \
                     MAX_SUBSCRIBERS cap). Each one holds an fd and up to \
                     {SUBSCRIBER_BUFFER_BYTES} bytes of outbound buffer; the cap is what \
                     stops 8 idle connections from disabling `subscribe` for everyone"
                ),
            };
        }
        // §9: a session that is not tailed (it never existed, or it was reaped and
        // disposed) is an EXPLICIT ERROR — never an empty stream that looks like a
        // quiet one.
        let Some((path, tail)) = read_channel.tail_state(session) else {
            return Response::Error {
                ok: false,
                error: format!(
                    "campd is not tailing {session} — it never existed, or it ended and its \
                     stream file was disposed. A reaped stream cannot be replayed (§9): the \
                     bytes are gone with the file"
                ),
            };
        };
        let c = cursor.unwrap_or(tail);
        if c > tail {
            return Response::Error {
                ok: false,
                error: format!(
                    "cursor {c} is past the {tail} bytes campd has consumed from {session}. \
                     A cursor is a byte offset campd itself issued; it can never run ahead of \
                     what campd has drained"
                ),
            };
        }
        // ORDINARY HISTORY IS NOT AN ERROR (D6"): a late joiner simply starts with
        // a low cursor and catches up.
        let file = match std::fs::OpenOptions::new().read(true).open(&path) {
            Ok(f) => f,
            Err(e) => {
                return Response::Error {
                    ok: false,
                    error: format!("opening {session}'s stream file {}: {e}", path.display()),
                };
            }
        };
        self.next_subscription += 1;
        let id = format!("sub-{}", self.next_subscription);
        self.subscribers.insert(
            token,
            Subscriber {
                id: id.clone(),
                out: OutBuf::new(),
                source: Source::File(FileSource {
                    session: session.to_owned(),
                    file,
                    // The invariant `cursor <= scan <= tail`, from birth.
                    cursor: c,
                    scan: c,
                    partial: Vec::new(),
                    held: false, // B3: a REAL flag, never a `partial` inspection
                    oversize: None,
                    tail,
                    closing: None,
                    end_sent: false,                                 // R2
                    frame_overhead: measure_frame_overhead(session), // R3: MEASURED
                }),
            },
        );
        Response::Subscribed {
            ok: true,
            v: 1,
            subscription: id,
            cursor: c,
        }
    }

    /// D6": refresh every subscriber's `tail` from the read channel, then pump.
    /// It no longer touches `lines` at all — a "live" line is just `tail`
    /// advancing.
    ///
    /// Returns the tokens to close and the durable events (`subscriber.dropped`
    /// plus any `over_cap` `patrol.degraded` from the collector).
    pub fn fanout(
        &mut self,
        ledger: &Ledger,
        patrol: &PatrolRuntime,
        read_channel: &ReadChannelRuntime,
        conns: &mut HashMap<Token, Conn>,
        now: Timestamp,
    ) -> (Vec<Token>, Vec<EventInput>) {
        // §4.3: recompute the fleet model ONCE per wake, and ONLY when a fleet
        // subscriber exists — computing it with nobody watching is pure waste
        // (invariant 1).
        if self.has_fleet_subscribers() {
            match self.fleet_model(ledger, patrol, read_channel) {
                Ok(model) => self.fleet_model = model,
                Err(e) => self.pending_events.push(degraded_event(
                    "(fleet)",
                    format!("fleet model refresh: {e}"),
                )),
            }
        }
        let cap = self.subscriber_buffer_bytes;
        let stall = self.stall_timeout;
        // Take the model so the pump loop can borrow it immutably while
        // `&mut self.pending_events`/`&mut self.degraded_seen` stay disjoint.
        let model = std::mem::take(&mut self.fleet_model);
        let mut gone = Vec::new();
        let mut events = Vec::new();

        let tokens: Vec<Token> = self.subscribers.keys().copied().collect();
        for token in tokens {
            let Some(sub) = self.subscribers.get_mut(&token) else {
                continue;
            };
            // The tail refresh, specified for all three cases (FILE sources only —
            // a fleet source has no tail).
            match &mut sub.source {
                Source::File(fs) => match read_channel.tail_state(&fs.session) {
                    // A CLOSING subscriber's tail is PINNED at the final offset,
                    // whatever `tail_state` now says.
                    //
                    // HONESTLY: also defence in depth. Deleting this arm survives the
                    // suite — once a session is disposed `tail_state` returns None,
                    // which the `None` arm below already leaves `tail` untouched. It
                    // stays because a future phase could make `tail_state` answer for
                    // a disposed session; it is not gated today.
                    _ if fs.closing.is_some() => {}
                    Some((_, t)) => fs.tail = t,
                    // The session is no longer tailed. LEAVE `tail` UNCHANGED — never
                    // zero it, never panic. This is the window between `dispose_pending`
                    // and `close_disposed` within ONE wake, and `close_disposed` pins
                    // the authoritative value immediately after.
                    None => {}
                },
                // A fleet source has no tail to refresh.
                Source::Fleet(_) => {}
            }
            let Some(conn) = conns.get_mut(&token) else {
                gone.push(token);
                continue;
            };
            match pump_subscriber(
                sub,
                conn,
                now,
                cap,
                stall,
                &mut self.pending_events,
                &mut self.degraded_seen,
                &model,
            ) {
                PumpOutcome::Ok => {}
                PumpOutcome::Gone => gone.push(token),
                PumpOutcome::Drop(event) => {
                    events.push(event);
                    gone.push(token);
                }
            }
        }
        self.fleet_model = model; // restore
        events.append(&mut self.pending_events);
        (gone, events)
    }

    /// Pump ONE subscriber (the WRITABLE-edge path, and the hello's first bytes).
    pub fn pump(&mut self, token: Token, conn: &mut Conn, now: Timestamp) -> PumpOutcome {
        // Take/restore the cached model so a resumed cap-STOPped fleet delta diffs
        // against the SAME model `fanout` computed (this path has no ledger in
        // scope). A forgotten restore would re-fill against an empty model → a
        // spurious `gone` per row.
        let model = std::mem::take(&mut self.fleet_model);
        let outcome = self.pump_inner(token, conn, now, &model);
        self.fleet_model = model; // restore
        outcome
    }

    fn pump_inner(
        &mut self,
        token: Token,
        conn: &mut Conn,
        now: Timestamp,
        model: &[SessionInfo],
    ) -> PumpOutcome {
        let cap = self.subscriber_buffer_bytes;
        let stall = self.stall_timeout;
        let Some(sub) = self.subscribers.get_mut(&token) else {
            return PumpOutcome::Gone;
        };
        pump_subscriber(
            sub,
            conn,
            now,
            cap,
            stall,
            &mut self.pending_events,
            &mut self.degraded_seen,
            model,
        )
    }

    /// Drain the durable events `pump` collected (it cannot take `&mut Ledger` —
    /// it is called with a `&mut Conn` already borrowed out of the same map).
    pub fn take_pending_events(&mut self) -> Vec<EventInput> {
        std::mem::take(&mut self.pending_events)
    }

    /// B12/C7/G4: the sessions the read channel just DISPOSED.
    ///
    /// Called from the event loop AFTER `dispose_pending` — never from inside
    /// `control_step`. That ordering is the whole fix: consuming the disposed list
    /// BEFORE `dispose_pending` produced it leaves it empty, `closing` is never
    /// set, and a subscriber that is exactly CAUGHT UP (the steady state of every
    /// long-lived watch) gets NO end frame and NO EOF, ever.
    pub fn close_disposed(
        &mut self,
        disposed: &[Disposed],
        ledger: &Ledger,
        conns: &mut HashMap<Token, Conn>,
        now: Timestamp,
    ) -> (Vec<Token>, Vec<EventInput>) {
        let cap = self.subscriber_buffer_bytes;
        let stall = self.stall_timeout;
        let mut gone = Vec::new();
        let mut events = Vec::new();

        for d in disposed {
            // `stopped` or `crashed` — and NOTHING ELSE may ever reach the wire.
            //
            // NOT `capped` (that value does not exist in the sessions table's status
            // column), and NOT `live` either: `close_disposed` runs on the disposal
            // wake, and if the status row has not settled yet a raw read yields
            // "live" — an `end` frame whose reason says the session is still running.
            // A phantom value is a contract nobody can honour, so the mapping is
            // TOTAL: crashed is crashed, everything else is stopped.
            let reason = match ledger.session_status(&d.session).ok().flatten().as_deref() {
                Some("crashed") => "crashed".to_owned(),
                _ => "stopped".to_owned(),
            };

            let tokens: Vec<Token> = self
                .subscribers
                .iter()
                .filter(|(_, s)| matches!(&s.source, Source::File(fs) if fs.session == d.session))
                .map(|(t, _)| *t)
                .collect();

            for token in tokens {
                let Some(sub) = self.subscribers.get_mut(&token) else {
                    continue;
                };
                // Only FILE subscribers are tied to a disposed session (the filter
                // above already guarantees this arm).
                match &mut sub.source {
                    Source::File(fs) => {
                        fs.closing = Some(reason.clone());
                        // The AUTHORITATIVE end of the stream (Task 4's `dispose_pending`
                        // recorded it). The `end` frame's offset comes from HERE.
                        //
                        // HONESTLY: this line is DEFENCE IN DEPTH, not a gated invariant.
                        // Deleting it survives the whole suite, because `fanout` has already
                        // refreshed `tail` from `tail_state` earlier in the SAME wake and the
                        // two agree. It stays because the ordering it relies on is a property
                        // of the event loop, not of this function — but do not call it tested.
                        fs.tail = d.final_offset;
                    }
                    // A fleet subscriber is not tied to a disposed session.
                    Source::Fleet(_) => {}
                }
                let Some(conn) = conns.get_mut(&token) else {
                    gone.push(token);
                    continue;
                };
                // A CAUGHT-UP subscriber hits the TERMINAL branch immediately and its
                // `end` frame goes out ON THIS WAKE.
                match pump_subscriber(
                    sub,
                    conn,
                    now,
                    cap,
                    stall,
                    &mut self.pending_events,
                    &mut self.degraded_seen,
                    &[], // a disposal wake targets file subscribers; fleet is pumped by fanout
                ) {
                    PumpOutcome::Ok => {}
                    PumpOutcome::Gone => gone.push(token),
                    PumpOutcome::Drop(event) => {
                        events.push(event);
                        gone.push(token);
                    }
                }
            }

            // G7: the session is gone — expire its still-pending control requests
            // LOUDLY, and prune its settled ones.
            events.extend(self.forget_session(&d.session, now));
        }
        events.append(&mut self.pending_events);
        (gone, events)
    }

    /// Drop a subscription. EVERY close path calls this — a normal detach appends
    /// NO event (§5.2: it is not a fault).
    pub fn forget(&mut self, token: Token) {
        self.subscribers.remove(&token);
    }

    pub fn is_subscriber(&self, token: Token) -> bool {
        self.subscribers.contains_key(&token)
    }

    #[allow(dead_code)] // PERMANENT: test observable (the read_channel.rs:445 precedent)
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }
}

// ---- the `pump` unit harness (the dispatch::test_insert_held_cat precedent) --

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
impl ControlRuntime {
    /// Insert a subscriber directly over a `UnixStream::pair()`, so `pump` can be
    /// driven with NO daemon, NO socket and NO timing.
    ///
    /// Returns the CLIENT end (the test reads it) and the `Conn` (the test passes
    /// it back into `pump`). **A test that never reads its client end IS a stalled
    /// peer** — which is how the stall drop and the terminal branch are exercised
    /// deterministically.
    pub fn test_insert_subscriber(
        &mut self,
        token: Token,
        session: &str,
        file: std::fs::File,
        cursor: u64,
        tail: u64,
    ) -> (std::os::unix::net::UnixStream, Conn) {
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        // The CLIENT is non-blocking too, so test readers never need `setsockopt`
        // on a live socket (which flakes with EINVAL under parallel load) and never
        // block forever on a stream that has stopped.
        client.set_nonblocking(true).unwrap();
        let conn = Conn {
            stream: mio::net::UnixStream::from_std(server),
            buf: Vec::new(),
        };
        self.next_subscription += 1;
        self.subscribers.insert(
            token,
            Subscriber {
                id: format!("sub-{}", self.next_subscription),
                out: OutBuf::new(),
                source: Source::File(FileSource {
                    session: session.to_owned(),
                    file,
                    cursor,
                    scan: cursor,
                    partial: Vec::new(),
                    held: false,
                    oversize: None,
                    tail,
                    closing: None,
                    end_sent: false,
                    frame_overhead: measure_frame_overhead(session),
                }),
            },
        );
        (client, conn)
    }

    /// The tests live in THIS module, so this needs no visibility at all —
    /// `Subscriber` is private and must stay that way (its invariants are `pump`'s).
    #[cfg(test)]
    fn test_sub(&self, token: Token) -> &Subscriber {
        &self.subscribers[&token]
    }

    /// Insert a FLEET subscriber directly over a `UnixStream::pair()`, so the
    /// model-diff pump can be driven with NO daemon and NO ledger.
    pub fn test_insert_fleet_subscriber(
        &mut self,
        token: Token,
    ) -> (std::os::unix::net::UnixStream, Conn) {
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        client.set_nonblocking(true).unwrap();
        let conn = Conn {
            stream: mio::net::UnixStream::from_std(server),
            buf: Vec::new(),
        };
        self.next_subscription += 1;
        self.subscribers.insert(
            token,
            Subscriber {
                id: format!("fleet-{}", self.next_subscription),
                out: OutBuf::new(),
                source: Source::Fleet(FleetSource::new()),
            },
        );
        (client, conn)
    }

    /// Drive ONE subscriber's pump against an explicit fleet model at an explicit
    /// `now` (production supplies the model through `fanout`; this is the unit
    /// entry, and `now` is explicit so stall-window tests are deterministic).
    pub fn pump_with_model(
        &mut self,
        token: Token,
        conn: &mut Conn,
        now: Timestamp,
        model: &[SessionInfo],
    ) -> PumpOutcome {
        let cap = self.subscriber_buffer_bytes;
        let stall = self.stall_timeout;
        let Some(sub) = self.subscribers.get_mut(&token) else {
            return PumpOutcome::Gone;
        };
        pump_subscriber(
            sub,
            conn,
            now,
            cap,
            stall,
            &mut self.pending_events,
            &mut self.degraded_seen,
            model,
        )
    }
}
