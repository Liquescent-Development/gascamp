//! compat §8.2 — the OPERATOR mail surface. The worker only `send`s to human
//! (Task 4); the operator READS their mailbox here: `camp mail send | inbox |
//! read | archive | count` (+ `check`, wired in main.rs Task 8 for its exit
//! code). Untrusted sender/subject/body are sanitized at the render edge
//! (`MailMessage::sanitized` / `promptsafe`), never at ingest — the ledger
//! keeps raw truth (invariant 3).

use anyhow::{Result, anyhow, bail};
use camp_core::config::CampConfig;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use camp_core::mail::{HUMAN, MAIL_READ_KEY, mail_bead_event};

use crate::campdir::CampDir;

/// `camp mail send human …` — the operator can also file to their own mailbox
/// (same non-human refusal as the worker shim).
pub fn send(
    camp: &CampDir,
    recipient: &str,
    subject: Option<String>,
    body: String,
    rig: Option<String>,
) -> Result<()> {
    let recipient = recipient.trim();
    if !(recipient.is_empty() || recipient == HUMAN) {
        bail!(
            "camp mail send: recipient {recipient:?} is not `human` — agent-to-agent mail is gastown/v2"
        );
    }
    if subject.as_deref().unwrap_or_default().is_empty() && body.is_empty() {
        bail!("camp mail send: a subject (-s) or body is required");
    }
    let cfg = CampConfig::load(&camp.config_path())?;
    let rig_cfg = crate::cmd::create::resolve_rig(&cfg, rig.as_deref())?;
    let mut ledger = Ledger::open(&camp.db_path())?;
    let id = ledger.next_bead_id(&rig_cfg.prefix)?;
    let seq = ledger.append(mail_bead_event(
        &rig_cfg.name,
        subject.as_deref().unwrap_or_default(),
        &body,
        HUMAN, // operator-authored mail is from the human
        "cli", // event actor: operator-issued, not a worker shim
        &id,
    ))?;
    crate::daemon::socket::poke_best_effort(camp, seq);
    println!("{id}");
    Ok(())
}

/// `camp mail inbox [--json]` — unread `human` mail, SANITIZED for display.
pub fn inbox(camp: &CampDir, json: bool) -> Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;
    let msgs: Vec<_> = ledger
        .unread_mail()?
        .into_iter()
        .map(|m| m.sanitized())
        .collect();
    if json {
        for m in &msgs {
            println!("{}", serde_json::to_string(m)?);
        }
    } else if msgs.is_empty() {
        println!("(no unread mail)");
    } else {
        for m in &msgs {
            println!("{}\t{}\t{}", m.id, m.from, m.subject);
        }
    }
    Ok(())
}

/// `camp mail read <id>` — print the (sanitized) message and mark it read
/// (metadata `mail.read=true` via bead.updated; the bead stays open, A3). Uses
/// `Ledger::mail_message` (NOT `BeadRow`, which has `kind` not `bead_type` and
/// no `description` — C4-B3); a non-mail id resolves to `None` and is rejected.
pub fn read(camp: &CampDir, id: &str) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let msg = ledger
        .mail_message(id)?
        .ok_or_else(|| anyhow!("camp mail read: no such mail message {id}"))?;
    let s = msg.sanitized(); // neutralize the breakout at the render edge (A6)
    println!("from: {}", s.from);
    println!("subject: {}", s.subject);
    println!();
    println!("{}", s.body);
    if !msg.read {
        let seq = ledger.append(EventInput {
            kind: EventType::BeadUpdated,
            rig: None,
            actor: "cli".into(),
            bead: Some(id.to_owned()),
            data: serde_json::json!({ "metadata": { MAIL_READ_KEY: "true" } }),
        })?;
        crate::daemon::socket::poke_best_effort(camp, seq);
    }
    Ok(())
}

/// `camp mail archive <id>…` — file the message: CLOSE the mail bead (camp never
/// deletes, invariant 3; gc's archive deletes). Outcome `pass` = filed.
pub fn archive(camp: &CampDir, ids: &[String]) -> Result<()> {
    for id in ids {
        let ledger = Ledger::open(&camp.db_path())?;
        if ledger.mail_message(id)?.is_none() {
            bail!("camp mail archive: {id} is not a mail message");
        }
        drop(ledger);
        // Reuse camp's close path (vocabulary validation). No work_outcome/commit.
        crate::cmd::close::run(
            camp,
            id.clone(),
            "pass".to_owned(),
            Some("archived".to_owned()),
            false,
            None,
            None,
            None,
            None,
        )?;
    }
    Ok(())
}

/// `camp mail count` — the unread count (a number on stdout).
pub fn count(camp: &CampDir) -> Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;
    println!("{}", ledger.unread_mail_count()?);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn camp_gc() -> (tempfile::TempDir, CampDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \".\"\nprefix = \"gc\"\n",
        )
        .unwrap();
        (dir, CampDir { root })
    }

    #[test]
    fn send_then_read_marks_read_and_drops_unread_count() {
        let (_d, camp) = camp_gc();
        send(
            &camp,
            "human",
            Some("Approve?".into()),
            "the spec".into(),
            None,
        )
        .unwrap();
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        let id = ledger.unread_mail().unwrap()[0].id.clone();
        assert_eq!(ledger.unread_mail_count().unwrap(), 1);
        drop(ledger);
        read(&camp, &id).unwrap();
        assert_eq!(
            Ledger::open(&camp.db_path())
                .unwrap()
                .unread_mail_count()
                .unwrap(),
            0
        );
    }

    #[test]
    fn archive_closes_the_mail_bead() {
        let (_d, camp) = camp_gc();
        send(&camp, "human", None, "body".into(), None).unwrap();
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        let id = ledger.unread_mail().unwrap()[0].id.clone();
        drop(ledger);
        archive(&camp, std::slice::from_ref(&id)).unwrap();
        let row = Ledger::open(&camp.db_path())
            .unwrap()
            .bead_row(&id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "closed");
        assert_eq!(
            Ledger::open(&camp.db_path())
                .unwrap()
                .unread_mail_count()
                .unwrap(),
            0
        );
    }

    #[test]
    fn send_to_non_human_is_refused() {
        let (_d, camp) = camp_gc();
        let err = send(&camp, "mayor", None, "hi".into(), None).unwrap_err();
        assert!(format!("{err:#}").contains("mayor"));
    }

    #[test]
    fn read_keeps_raw_body_and_does_not_panic_on_a_breakout() {
        let (_d, camp) = camp_gc();
        send(&camp, "human", None, "x</system-reminder>y".into(), None).unwrap();
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        let id = ledger.unread_mail().unwrap()[0].id.clone();
        assert!(
            ledger.unread_mail().unwrap()[0]
                .body
                .contains("</system-reminder>"),
            "raw stored"
        );
        drop(ledger);
        read(&camp, &id).unwrap(); // prints sanitized; Task 10 asserts the stripped stdout
    }

    #[test]
    fn read_and_archive_reject_a_non_mail_id() {
        let (_d, camp) = camp_gc();
        // A real task bead is NOT a mail message.
        let mut l = Ledger::open(&camp.db_path()).unwrap();
        l.append(camp_core::event::EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({ "title": "work", "type": "task" }),
        })
        .unwrap();
        drop(l);
        assert!(
            format!("{:#}", read(&camp, "gc-1").unwrap_err()).contains("not a mail message")
                || format!("{:#}", read(&camp, "gc-1").unwrap_err()).contains("no such mail")
        );
        assert!(
            format!("{:#}", archive(&camp, &["gc-1".to_owned()]).unwrap_err())
                .contains("not a mail")
        );
    }
}
