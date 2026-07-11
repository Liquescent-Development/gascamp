#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A GATE, not a convention (Phase 2, campd service management).
//!
//! `camp init` now installs a REAL host service unit wherever a service manager
//! exists (design §6). A test that runs a bare `camp init` therefore writes a
//! real LaunchAgent into the developer's — or the macos-latest runner's —
//! ~/Library/LaunchAgents and starts a real campd against a temp directory that
//! is about to be deleted. Every camp-init call in this suite MUST pass
//! --no-service, and this test is what keeps that true as tests are added.
//!
//! A call site naming `"init"` on an `.arg(…)`/`.args([…])` chain passes only
//! if it carries ONE of three things. The two markers are NOT interchangeable,
//! and a marker that does not describe the call it sits on is a lie that
//! defeats the gate:
//!
//!   `--no-service`     the normal case: a camp init that installs nothing.
//!                       Must appear as an actual quoted argument
//!                       (`"--no-service"`) in the CALL ITSELF — a comment
//!                       that merely mentions the flag does not count (a
//!                       stray `// TODO: --no-service` next to a bare
//!                       `.arg("init")` is exactly the bug this gate exists
//!                       to catch, not an excuse for it).
//!
//!   `// not-camp:`     it is not the camp binary at all. This suite also runs
//!                      `git init` (many files) and one `bd init`
//!                      (cli_export.rs). Nothing about camp applies to them.
//!
//!                      The marker must EARN it, not merely assert it: the
//!                      chunk has to construct a literally-named other program
//!                      (`Command::new("git")`, `Command::new("bd")` — how both
//!                      real sites are already written) and must not run the
//!                      camp binary (`cargo_bin("camp")`). Otherwise the
//!                      comment alone would excuse the chunk, and the same
//!                      comment lied onto a real `camp init` would wave a live
//!                      LaunchAgent straight through the gate.
//!
//!   `// real-manager:` a DELIBERATE bare `camp init` — the environment-aware
//!                      default (design §6) — which is legitimate ONLY inside a
//!                      test that is BOTH `#[ignore]`d AND gated on
//!                      CAMP_SERVICE_E2E=1, so `cargo test --workspace` and CI
//!                      never run it and only an operator who typed
//!                      `make service-e2e` can install anything. Today there is
//!                      exactly one: the real-manager lifecycle test in
//!                      cli_service.rs, whose whole purpose is to prove that
//!                      `camp init` DOES install a unit on a host that has a
//!                      service manager. If you reach for this marker anywhere
//!                      else, you are almost certainly writing the bug this
//!                      gate exists to catch: use --no-service instead.
//!
//!                      Because the marker cannot verify its own precondition
//!                      just by sitting on a line, this scan additionally
//!                      requires that the marker's OWN enclosing test function
//!                      carry `#[ignore]` — not merely that the file contain
//!                      an `#[ignore` somewhere, which a second, non-ignored
//!                      bare init in the same file could otherwise ride along
//!                      with — AND that the file contain `CAMP_SERVICE_E2E`.
//!
//! THE SCAN IS OVER LOGICAL CALL CHAINS, NOT PHYSICAL LINES. rustfmt freely
//! moves `"init"`, `.args([`, `--no-service`, and a trailing `// marker:`
//! comment onto different physical lines of the same statement whenever a
//! chain crosses its width limit (it already does this elsewhere in this very
//! suite, e.g. cli_claim_close.rs, cli_create.rs) — a scan keyed to "does ONE
//! physical line carry both `"init"` and `.arg(`" would go blind the moment
//! that happens to a call site here, with no human intent required. So: each
//! file is first split into logical units — a run of physical lines joined by
//! tracking paren/bracket nesting (masking string-literal interiors and `//`
//! comments first, so a bracket inside either can't perturb the count) until
//! nesting returns to zero AND the line ends `;`, `{`, or `}` — and the
//! `"init"`/`.arg(`/`--no-service` predicate runs against the JOINED unit, not
//! a single line. A violation is still reported at the precise physical line
//! that names `"init"`, not just "somewhere in this multi-line statement".
//!
//! Known, accepted limits of that join (not a parser, and not meant to be
//! one):
//!   - It has no notion of a match arm: a comma-terminated expression at
//!     paren/bracket depth zero keeps a unit open until the next `;`/`{`/`}`,
//!     so several arms between two block delimiters join into one unit. No
//!     camp-init call in this suite sits inside a match arm; if one ever
//!     does, give it its own `;`-terminated statement so the join stays
//!     precise.
//!   - Block comments (`/* … */`) are not specially masked — only `//` line
//!     comments are. This suite uses no block comments.
//!   - The `#[ignore]`-binding scan (for `// real-manager:`) finds a test
//!     function's own attributes by walking physical lines too: it looks for
//!     a line beginning (after `pub`/`async`/`unsafe`/`const`) with `fn `,
//!     and treats the contiguous `#[...]`/doc-comment lines directly above it
//!     as that function's attributes. A `#[ignore = "…"]` attribute that
//!     itself got split across physical lines would not be recognized —
//!     accepted, since rustfmt does not reflow attribute string literals the
//!     way it reflows a `.args([…])` list. It also assumes no nested `fn`
//!     items inside a test body (none exist in this suite); brace-depth
//!     tracking (also mask-aware) is what lets it tell a fn's own body apart
//!     from the next top-level item, so blocks/closures *inside* a test body
//!     never reset which function's `#[ignore]` is in scope.
//!   - It is a TEXTUAL scan for the literal argument `"init"`, not a compiler:
//!     an argv built from a `const` or a variable and handed to `.arg(…)`
//!     (e.g. `const INIT: &str = "init"; cmd.arg(INIT)`) never spells the
//!     text `"init"` at the call site, so this scan is blind to it — the
//!     call would run a bare `camp init` with no `--no-service` and no
//!     marker, and pass silently.
//!   - Likewise for a macro: an argv assembled inside a macro invocation that
//!     never spells `.arg(`/`.args(` as source text in this file (e.g. a
//!     helper macro that expands to an `.arg(...)` call the scan never sees
//!     verbatim) is invisible too, for the same reason — the predicate looks
//!     for the literal tokens `"init"` and `.arg(`/`.args(` in this file's
//!     text, not for what any macro expands to.
//!
//! The scan is also NON-RECURSIVE: it reads only the files directly inside
//! `crates/camp/tests/`, not subdirectories (e.g. `tests/fixtures/`). Today
//! nothing under a subdirectory is a `.rs` file that could hide a call site;
//! if that ever changes, this scan must be taught to recurse. It likewise
//! never looks inside `crates/camp/src/**` — a `#[cfg(test)]` unit test
//! living next to the code it tests is invisible to this gate no matter what
//! it shells out to. Today that is exhaustive: every camp-init call site this
//! suite needs already lives under `crates/camp/tests/`. If a future `src/`
//! test ever shells out to `camp init`, it would bypass this gate silently —
//! this scan would need to be taught to look there too.

use std::path::Path;

/// Splits `line` into (`mask`, `code`). Both drop anything after an unquoted
/// `//` (this suite has no block comments). `mask` additionally blanks
/// string-literal interiors — so a bracket character inside a string can't
/// perturb bracket-depth counting — while `code` keeps string contents intact,
/// so a quoted argument like `"init"`/`"--no-service"` can still be found.
/// Escaped quotes (`\"`) inside a string do not end it.
fn scan_line(line: &str) -> (String, String) {
    let mut mask = String::with_capacity(line.len());
    let mut code = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    let mut in_string = false;
    while let Some((_, c)) = chars.next() {
        if in_string {
            code.push(c);
            mask.push(' ');
            if c == '\\' {
                if let Some(&(_, escaped)) = chars.peek() {
                    code.push(escaped);
                    mask.push(' ');
                    chars.next();
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            code.push(c);
            mask.push(' ');
            continue;
        }
        if c == '/' && chars.peek().map(|&(_, next)| next) == Some('/') {
            break; // the rest of the line is a line comment: drop it from both
        }
        code.push(c);
        mask.push(c);
    }
    (mask, code)
}

/// Whether `trimmed` begins a fn item — after stripping the modifiers Rust
/// allows before `fn` — used only to find the physical line a test function's
/// signature starts on, so `#[ignore]` can be bound to the right function.
fn starts_fn_signature(trimmed: &str) -> bool {
    let mut rest = trimmed;
    loop {
        let stripped = ["pub(crate) ", "pub ", "async ", "unsafe ", "const "]
            .iter()
            .find_map(|prefix| rest.strip_prefix(prefix));
        match stripped {
            Some(r) => rest = r,
            None => break,
        }
    }
    rest.starts_with("fn ")
}

/// For every physical line, whether that line lies at or inside a test
/// function whose OWN attributes (the contiguous `#[...]`/doc-comment lines
/// directly above its `fn` line) include `#[ignore`. Brace depth (string/
/// comment-masked) tracks where one fn's body ends and the next top-level
/// item begins, so a block or closure inside a test body never resets it.
fn ignore_scope(lines: &[&str]) -> Vec<bool> {
    let mut result = vec![false; lines.len()];
    let mut brace_depth: i64 = 0;
    let mut pending_ignore = false;
    let mut current_fn_ignored = false;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let (mask, _code) = scan_line(line);
        if trimmed.starts_with("#[") {
            if trimmed.contains("#[ignore") {
                pending_ignore = true;
            }
        } else if trimmed.starts_with("///") || trimmed.starts_with("//!") || trimmed.is_empty() {
            // Doc comments/blank lines between attributes and `fn` don't
            // break the chain of attributes belonging to that fn.
        } else if brace_depth == 0 && starts_fn_signature(trimmed) {
            current_fn_ignored = pending_ignore;
            pending_ignore = false;
        } else if brace_depth == 0 {
            pending_ignore = false;
        }
        result[i] = current_fn_ignored;
        for c in mask.chars() {
            match c {
                '{' => brace_depth += 1,
                '}' => brace_depth -= 1,
                _ => {}
            }
        }
    }
    result
}

/// One logical call-chain statement, joined back together across however many
/// physical lines rustfmt put it on.
struct Chunk {
    /// The chunk's first physical line — lets a marker found at some offset
    /// into `raw` be translated back to an absolute file line.
    start: usize,
    /// The physical line the reporter should point at: the first line in the
    /// chunk that actually names `"init"`, or the chunk's first line if none
    /// do (never surfaced, since the violation predicate requires `"init"`).
    report_line: usize,
    /// Comment-stripped, string-preserved text — safe to search for a quoted
    /// `"init"`/`"--no-service"` argument without a comment forging a match.
    code: String,
    /// Untouched text (comments intact) — the markers live in comments, so
    /// this is what marker detection searches.
    raw: String,
}

/// Joins `lines` into logical units by tracking paren/bracket nesting
/// (string/comment-masked) until it returns to zero AND the line ends `;`,
/// `{`, or `}` — see the module docs for why a chain can't simply end at
/// bracket-depth zero (e.g. `camp()` alone is balanced but the chain
/// continues on the next line).
fn chunks(lines: &[&str]) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut depth: i64 = 0;
    let mut raw = String::new();
    let mut code = String::new();
    let mut init_line: Option<usize> = None;
    let mut start = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if raw.is_empty() {
            start = i;
        }
        raw.push_str(line);
        raw.push('\n');
        let (mask, line_code) = scan_line(line);
        if init_line.is_none() && line_code.contains("\"init\"") {
            init_line = Some(i);
        }
        code.push_str(&line_code);
        code.push('\n');
        for c in mask.chars() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                _ => {}
            }
        }
        let mask_trim = mask.trim_end();
        let boundary =
            mask_trim.ends_with(';') || mask_trim.ends_with('{') || mask_trim.ends_with('}');
        if depth == 0 && boundary {
            out.push(Chunk {
                start,
                report_line: init_line.take().unwrap_or(start),
                code: std::mem::take(&mut code),
                raw: std::mem::take(&mut raw),
            });
        }
    }
    if !raw.is_empty() {
        out.push(Chunk {
            start,
            report_line: init_line.unwrap_or(start),
            code,
            raw,
        });
    }
    out
}

#[test]
fn no_test_invokes_camp_init_without_no_service() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut violations = Vec::new();

    for entry in std::fs::read_dir(&tests).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // This file quotes the very patterns it forbids.
        if path.file_name().and_then(|n| n.to_str()) == Some("no_bare_camp_init.rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
        let file_gated = source.contains("CAMP_SERVICE_E2E");
        let lines: Vec<&str> = source.lines().collect();
        let fn_ignored_at = ignore_scope(&lines);

        for chunk in chunks(&lines) {
            let names_init = chunk.code.contains("\"init\"");
            let is_arg = chunk.code.contains(".arg(") || chunk.code.contains(".args(");
            if !(names_init && is_arg) {
                continue;
            }
            let has_no_service = chunk.code.contains("\"--no-service\"");
            let not_camp = chunk.raw.contains("not-camp:");
            let real_manager = chunk.raw.contains("real-manager:");
            if !(has_no_service || not_camp || real_manager) {
                violations.push(format!(
                    "{file_name}:{}: {}",
                    chunk.report_line + 1,
                    lines[chunk.report_line].trim()
                ));
                continue;
            }
            if not_camp {
                // M4 (review round 1). `not-camp:` was pure honor system: a
                // comment asserting "this isn't the camp binary" excused the
                // chunk outright, so the same comment lied onto a REAL
                // `camp init` would wave a live LaunchAgent straight through.
                // Make the marker prove itself from the code instead: a chunk
                // that is genuinely not camp names some OTHER program
                // literally (`Command::new("git")`, `Command::new("bd")` — how
                // both real sites are written), and a chunk that runs the camp
                // binary cannot excuse itself no matter what its comment says.
                let runs_camp_binary = chunk.code.contains("cargo_bin(\"camp\")");
                let names_another_program = chunk.code.contains("Command::new(\"")
                    && !chunk.code.contains("Command::new(\"camp\")");
                if runs_camp_binary || !names_another_program {
                    violations.push(format!(
                        "{file_name}:{}: carries `not-camp:` but the code does not bear it out — \
                         a not-camp chunk must construct a literally-named OTHER program \
                         (e.g. `Command::new(\"git\")`) and must not run the camp binary: {}",
                        chunk.report_line + 1,
                        lines[chunk.report_line].trim()
                    ));
                    continue;
                }
            }
            if real_manager {
                // The marker's precondition can't be checked from the chunk
                // alone: find the marker's OWN physical line and require
                // that its enclosing test function itself carry #[ignore],
                // plus that the file be CAMP_SERVICE_E2E-gated.
                let marker_line = chunk
                    .raw
                    .lines()
                    .position(|l| l.contains("real-manager:"))
                    .map_or(chunk.start, |offset| chunk.start + offset);
                let fn_ignored = fn_ignored_at.get(marker_line).copied().unwrap_or(false);
                if !(fn_ignored && file_gated) {
                    violations.push(format!(
                        "{file_name}:{}: carries `real-manager:` but its enclosing test \
                         function is not itself #[ignore]d and/or this file is not gated on \
                         CAMP_SERVICE_E2E — the marker's precondition does not hold: {}",
                        marker_line + 1,
                        lines[marker_line].trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "these lines run a bare `camp init`, which installs a REAL host service unit on any \
         machine that has a service manager (every dev mac; the macos-latest runner). Pass \
         --no-service. The only exemptions are `// not-camp:` (not the camp binary — git/bd) \
         and `// real-manager:` (a deliberate bare init inside an #[ignore]d, \
         CAMP_SERVICE_E2E-gated test); see this test's module docs before using either:\n{}",
        violations.join("\n")
    );
}
