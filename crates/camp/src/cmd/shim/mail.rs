//! compat §8.2 — the gc-shim `mail` verb. v1 serves the corpus's ACTUAL usage:
//! `send human` (all 10 corpus mail calls, A7) + `check` (the exit-code
//! contract, A2). Every OTHER recipient is refused naming gastown/v2; `--all`,
//! `--inject`, and `inbox`/`read`/`archive`/`count` are refused (invariant 1 +
//! operator-side surface). Refusals are LOUD (`shim.refused`).
//!
//! The worker context (`CAMP_BEAD`/`CAMP_SESSION`) is read ONCE at the env edge
//! (`send`) and INJECTED into the testable core (`send_with_context`) — the
//! codebase forbids `unsafe`, and edition-2024 `env::set_var` is `unsafe`, so
//! the core must be env-free to be unit-testable (invariant 5).

use anyhow::{Result, anyhow, bail};
use camp_core::ledger::Ledger;
use camp_core::mail::{HUMAN, mail_bead_event};

use super::{ShimExit, refuse};
use crate::campdir::CampDir;

/// `camp gc-shim mail <verb> …` dispatch.
pub fn run(camp: &CampDir, args: &[String]) -> Result<ShimExit> {
    match args.first().map(String::as_str) {
        Some("send") => send(camp, &args[1..]),
        Some("check") => check(camp, &args[1..]),
        Some(other @ ("inbox" | "read" | "archive" | "count" | "peek" | "reply")) => refuse(
            camp,
            &format!("mail {other}"),
            "reading/managing mail is the operator surface `camp mail` — a v1 worker has no mailbox",
        ),
        _ => refuse(
            camp,
            &format!("mail {}", args.first().map(String::as_str).unwrap_or("")),
            "gc mail shim serves only `send human` and `check` in v1",
        ),
    }
}

/// The env edge: read the dispatched-worker context and delegate. `CAMP_BEAD`
/// scopes the mail bead to the worker's rig; `CAMP_SESSION` names the sender.
/// Reading env is safe; only SETTING is `unsafe` (edition 2024) — so the core
/// takes these as parameters instead.
fn send(camp: &CampDir, args: &[String]) -> Result<ShimExit> {
    let worker_bead = std::env::var("CAMP_BEAD").ok();
    let session = std::env::var("CAMP_SESSION").ok();
    send_with_context(camp, args, worker_bead.as_deref(), session.as_deref())
}

/// gc's send grammar (A1), with the worker context INJECTED (no env reads). v1
/// accepts ONLY recipient `human` (or empty ⇒ human); anything else is refused
/// naming gastown/v2. `--all` is v2. The refusal + grammar arms are env-free
/// (testable directly); only the successful create needs `worker_bead`.
fn send_with_context(
    camp: &CampDir,
    args: &[String],
    worker_bead: Option<&str>,
    session: Option<&str>,
) -> Result<ShimExit> {
    let mut to: Option<String> = None;
    let mut subject: Option<String> = None;
    let mut message: Option<String> = None;
    let mut from: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--to" => to = Some(next_val(&mut it, camp, "--to")?),
            "-s" | "--subject" => subject = Some(next_val(&mut it, camp, "--subject")?),
            "-m" | "--message" => message = Some(next_val(&mut it, camp, "--message")?),
            "--from" => from = Some(next_val(&mut it, camp, "--from")?),
            "--notify" | "--nudge" => {} // no-op: v1 mail is to `human`, never nudged
            "--json" => {}               // accepted; the shim reply is silent success either way
            "--all" => {
                return refuse(
                    camp,
                    "mail send",
                    "`--all` broadcast to sessions is gastown/v2 — v1 mail is `send human` only",
                );
            }
            flag if flag.starts_with('-') => {
                return refuse(camp, "mail send", &format!("unknown flag {flag:?}"));
            }
            _ => positionals.push(a.clone()),
        }
    }

    let recipient = to
        .clone()
        .or_else(|| positionals.first().cloned())
        .unwrap_or_default();
    let recipient = recipient.trim();
    if !(recipient.is_empty() || recipient == HUMAN) {
        return refuse(
            camp,
            "mail send",
            &format!("recipient {recipient:?} is not `human` — agent-to-agent mail is gastown/v2"),
        );
    }
    // Body/subject mirror gc (A1). With --to, the whole positional vec is body;
    // otherwise the recipient is positionals[0] and body is the rest.
    let body = message.unwrap_or_else(|| {
        let start = usize::from(to.is_none());
        positionals
            .get(start..)
            .map(|s| s.join(" "))
            .unwrap_or_default()
    });
    let subject = subject.unwrap_or_default();
    if subject.is_empty() && body.is_empty() {
        bail!(
            "gc mail send: usage: gc mail send human <body>  OR  gc mail send human -s <subject> [-m <body>]"
        );
    }

    let worker_bead = worker_bead.ok_or_else(|| {
        anyhow!("gc mail send: CAMP_BEAD not set — the shim runs only inside a dispatched worker")
    })?;
    let sender = from
        .or_else(|| session.map(str::to_owned))
        .unwrap_or_else(|| HUMAN.to_owned());

    let mut ledger = Ledger::open(&camp.db_path())?;
    let rig = ledger
        .bead_row(worker_bead)?
        .ok_or_else(|| anyhow!("gc mail send: worker bead {worker_bead:?} not in ledger"))?
        .rig;
    let prefix = rig_prefix(camp, &rig)?;
    let id = ledger.next_bead_id(&prefix)?;
    let seq = ledger.append(mail_bead_event(&rig, &subject, &body, &sender, "gc-shim", &id))?;
    crate::daemon::socket::poke_best_effort(camp, seq);
    Ok(ShimExit(0))
}

/// gc's `mail check` exit-code contract (A2): exit 0 if unread mail exists, 1
/// if empty. `--inject` (the per-turn hook §11.2) is REFUSED — invariant 1.
pub(super) fn check(camp: &CampDir, args: &[String]) -> Result<ShimExit> {
    for a in args {
        match a.as_str() {
            "--inject" => {
                return refuse(
                    camp,
                    "mail check",
                    "`--inject` is the per-turn hook withdrawn to gastown/v2 (invariant 1 intact) — v1 has no agent recipient to inject into",
                );
            }
            "--hook-format" => {
                return refuse(camp, "mail check", "`--hook-format` is a v2 injection concern");
            }
            flag if flag.starts_with('-') => {
                return refuse(camp, "mail check", &format!("unknown flag {flag:?}"));
            }
            _ => {} // an optional [session] positional: v1 mailbox is always `human`
        }
    }
    let ledger = Ledger::open(&camp.db_path())?;
    let n = ledger.unread_mail_count()?;
    println!("{n}");
    Ok(ShimExit(if n > 0 { 0 } else { 1 }))
}

/// Pull the value token for a flag, or refuse (which always returns `Err`) so
/// the caller propagates the failure — no panic path.
fn next_val<'a>(
    it: &mut impl Iterator<Item = &'a String>,
    camp: &CampDir,
    flag: &str,
) -> Result<String> {
    match it.next() {
        Some(v) => Ok(v.clone()),
        // `refuse` always returns `Err`; `.map` re-types its (never-taken) `Ok`
        // arm to `String`, so this is the propagated refusal — no panic.
        None => refuse(camp, "mail send", &format!("{flag} needs a value")).map(|_| String::new()),
    }
}

/// The per-rig id prefix from camp.toml (for `next_bead_id`).
fn rig_prefix(camp: &CampDir, rig: &str) -> Result<String> {
    let cfg = camp_core::config::CampConfig::load(&camp.config_path())?;
    Ok(cfg.rig(rig)?.prefix.clone())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::event::{EventInput, EventType};

    const BEAD: &str = "gc-9";
    const SESSION: &str = "t/gc.publisher/1";

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    /// A camp with a `gc` rig + one open worker bead `gc-9`. The worker context
    /// is INJECTED into `send_with_context` (never via process env — `unsafe`
    /// `set_var` is forbidden, invariant 5).
    fn worker_camp() -> (tempfile::TempDir, CampDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \".\"\nprefix = \"gc\"\n",
        )
        .unwrap();
        let camp = CampDir { root };
        let mut l = Ledger::open(&camp.db_path()).unwrap();
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: Some(BEAD.into()),
            data: serde_json::json!({ "title": "work", "type": "task" }),
        })
        .unwrap();
        (dir, camp)
    }

    /// The successful-send helper: inject the worker context, no env.
    fn send_ok(camp: &CampDir, args: &[&str]) {
        send_with_context(camp, &s(args), Some(BEAD), Some(SESSION)).unwrap();
    }

    #[test]
    fn send_human_positional_body_creates_an_unread_mail_bead() {
        let (_d, camp) = worker_camp();
        send_ok(&camp, &["human", "please", "review", "PR", "42"]);
        let l = Ledger::open(&camp.db_path()).unwrap();
        let inbox = l.unread_mail().unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].body, "please review PR 42");
        assert_eq!(inbox[0].from, SESSION);
    }

    #[test]
    fn send_human_dash_s_dash_m_maps_subject_and_body() {
        let (_d, camp) = worker_camp();
        send_ok(&camp, &["human", "-s", "Spec approval", "-m", "please review"]);
        let l = Ledger::open(&camp.db_path()).unwrap();
        let inbox = l.unread_mail().unwrap();
        assert_eq!(inbox[0].subject, "Spec approval");
        assert_eq!(inbox[0].body, "please review");
    }

    #[test]
    fn send_via_to_flag_is_accepted() {
        let (_d, camp) = worker_camp();
        send_ok(&camp, &["--to", "human", "build is green"]);
        let l = Ledger::open(&camp.db_path()).unwrap();
        assert_eq!(l.unread_mail().unwrap()[0].body, "build is green");
    }

    #[test]
    fn send_from_flag_overrides_session_as_sender() {
        let (_d, camp) = worker_camp();
        send_with_context(
            &camp,
            &s(&["human", "--from", "t/gc.run-operator/1", "escalation"]),
            Some(BEAD),
            Some(SESSION),
        )
        .unwrap();
        let l = Ledger::open(&camp.db_path()).unwrap();
        assert_eq!(l.unread_mail().unwrap()[0].from, "t/gc.run-operator/1");
    }

    #[test]
    fn send_to_a_non_human_recipient_is_refused_naming_v2_and_makes_no_bead() {
        let (_d, camp) = worker_camp();
        // env-free: the recipient refusal fires before any worker-context use.
        let err = send_with_context(&camp, &s(&["mayor", "hi"]), None, None).unwrap_err();
        assert!(format!("{err:#}").contains("mayor"));
        let l = Ledger::open(&camp.db_path()).unwrap();
        assert!(
            l.events_of_type(EventType::ShimRefused)
                .unwrap()
                .iter()
                .any(|e| e.data["verb"] == "mail send")
        );
        assert!(l.unread_mail().unwrap().is_empty());
    }

    #[test]
    fn send_all_broadcast_is_refused_as_v2() {
        let (_d, camp) = worker_camp();
        let err = send_with_context(&camp, &s(&["--all", "status"]), None, None).unwrap_err();
        assert!(format!("{err:#}").contains("all") || format!("{err:#}").contains("gastown"));
    }

    #[test]
    fn managing_mail_on_the_worker_shim_is_refused() {
        let (_d, camp) = worker_camp();
        for verb in ["inbox", "read", "archive", "count"] {
            let err = run(&camp, &s(&[verb])).unwrap_err();
            assert!(format!("{err:#}").contains(verb));
        }
    }

    #[test]
    fn check_exits_1_on_empty_and_0_with_mail() {
        let (_d, camp) = worker_camp();
        assert_eq!(check(&camp, &[]).unwrap().0, 1, "empty inbox = exit 1 (A2)");
        send_ok(&camp, &["human", "hi"]);
        assert_eq!(check(&camp, &[]).unwrap().0, 0, "has mail = exit 0 (A2)");
    }

    #[test]
    fn check_inject_is_refused_invariant_1() {
        let (_d, camp) = worker_camp();
        let err = check(&camp, &s(&["--inject"])).unwrap_err();
        assert!(format!("{err:#}").contains("inject"));
        let l = Ledger::open(&camp.db_path()).unwrap();
        assert!(
            l.events_of_type(EventType::ShimRefused)
                .unwrap()
                .iter()
                .any(|e| e.data["verb"] == "mail check")
        );
    }

    #[test]
    fn documented_send_flags_pass_through_not_refused() {
        // gc's send grammar carries --json and --notify/--nudge (A1). If a
        // future edit dropped their accept arms, they'd fall into the
        // `starts_with('-')` refuse branch and silently break gc compat. This
        // test guards that: the mail bead is still created.
        let (_d, camp) = worker_camp();
        send_ok(
            &camp,
            &["human", "--json", "--notify", "-s", "Green", "-m", "build passed"],
        );
        let l = Ledger::open(&camp.db_path()).unwrap();
        let inbox = l.unread_mail().unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].subject, "Green");
        assert_eq!(inbox[0].body, "build passed");
        // No refusal was recorded for a documented flag.
        assert!(l.events_of_type(EventType::ShimRefused).unwrap().is_empty());
    }
}
