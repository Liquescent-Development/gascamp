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

use super::dispatch::{ControlWrite, Dispatcher, NudgeOutcome};
use super::read_channel::StreamLine;
use super::socket::Response;

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
    #[allow(dead_code)] // cp-1: first read in Task 8 (the subscriber hard cap)
    subscriber_buffer_bytes: usize,
}

impl ControlRuntime {
    pub fn new(subscriber_buffer_bytes: usize) -> ControlRuntime {
        ControlRuntime {
            pending: HashMap::new(),
            answered: HashMap::new(),
            timed_out: HashMap::new(),
            subscriber_buffer_bytes,
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

    /// The earliest due request, as a Duration from now. `None` when nothing is
    /// pending — an idle campd blocks forever (invariant 1).
    ///
    /// Task 8 extends this with the subscriber continuation.
    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration> {
        let earliest = self.pending.values().map(Pending::due_at).min()?;
        Some(duration_until(earliest, now))
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
    pub fn rehydrate(&mut self, ledger: &Ledger, now: Timestamp) -> anyhow::Result<usize> {
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

        // The liveness filter: only sessions that are still live can ever
        // produce another line, so only their requests can ever be corrected.
        let mut live: HashMap<String, bool> = HashMap::new();
        let mut restored = 0usize;

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
            if !alive {
                continue;
            }

            if responded.contains(&id) {
                self.answered.insert(id, p.session);
                continue;
            }
            match failed.get(&id).map(String::as_str) {
                // The two CORRECTABLE causes: campd said "no answer came", and
                // an answer may yet come.
                Some("silence_timeout") | Some("ceiling_timeout") => {
                    self.timed_out.insert(id, p);
                }
                Some(cause) if TERMINAL_CAUSES.contains(&cause) => {
                    self.answered.insert(id, p.session);
                }
                // An unrecognized cause is a HARD ERROR, never a default: a
                // value this campd does not know means the ledger was written by
                // a NEWER camp, and guessing its meaning is exactly the silent
                // divergence invariant 5 forbids.
                Some(unknown) => {
                    anyhow::bail!(
                        "control.failed for request_id {id} carries cause {unknown:?}, which \
                         this camp does not know. The ledger was written by a newer camp; \
                         guessing what that cause means would silently change how a late \
                         control_response is handled. Upgrade camp."
                    );
                }
                None => {
                    self.pending.insert(id, p);
                    restored += 1;
                }
            }
        }
        Ok(restored)
    }

    /// G7: the session was disposed. Its still-PENDING rows are EXPIRED LOUDLY
    /// — never silently dropped — and its `answered`/`timed_out` rows are
    /// PRUNED, which is what bounds both maps by LIVE sessions.
    ///
    /// A late answer cannot arrive after disposal: the session is no longer
    /// tailed, so there is nothing left to re-read.
    #[allow(dead_code)] // cp-1: first read in Task 8 (close_disposed)
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
            events.push(EventInput {
                kind: EventType::ControlFailed,
                rig: p.rig,
                actor: "campd".into(),
                bead: p.bead,
                data: serde_json::json!({
                    "session": p.session,
                    "request_id": id,
                    "verb": p.verb,
                    "cause": "session_ended",
                    "reason": format!(
                        "the session {session} ended with an unanswered control request \
                         (request_id {id}, {}). The most likely story is that the interrupt \
                         WORKED and the worker died before flushing its ack — but camp does \
                         not know that, so it says what it does know rather than nothing \
                         (invariant 3)",
                        p.verb
                    ),
                }),
            });
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

/// The causes from which NO answer can ever arrive. A `control_response` for a
/// request settled by one of these is a duplicate, never a correction.
const TERMINAL_CAUSES: &[&str] = &[
    "session_ended",
    "write_failed",
    "unknown_request",
    "unparsable",
    "dialog_refused",
    "permission_unanswerable",
];

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
            rt.rehydrate(&ledger, t0()).unwrap(),
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
}
