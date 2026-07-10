//! Bounded I/O for campd's single-threaded event loop (issue #55): every
//! blocking syscall the loop runs inline must carry a deadline, or one
//! stuck pipe / hung subprocess wedges the whole daemon — no dispatch, no
//! SIGCHLD reaping, no socket service — while looking exactly like an
//! idle one (invariant 1 means no heartbeat, and the kernel listen
//! backlog keeps accepting connects). `write_bounded` is the PR #51
//! precedent (review finding 2), moved here; `output_bounded` extends the
//! same discipline to the subprocesses campd runs on the loop (git
//! worktree ops, adoption probes, /bin/kill). Every deadline is a bound
//! on the loop's worst-case stall, not a wakeup — nothing ticks while
//! nothing runs (invariant 1).

use std::io::{ErrorKind, Read as _, Write as _};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long one stream-json line write into a worker's held stdin may
/// take before campd declares the pipe wedged (PR #51 review finding 2:
/// the nudge path; issue #55 extends it to the task write at launch). A
/// fresh pipe swallows a task without blocking, so on the launch path the
/// bound only ever fires on a task larger than the OS pipe buffer fed to
/// a worker that never started reading.
pub const STDIN_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounded write of one stream-json line into a worker's held stdin:
/// non-blocking writes; on WouldBlock, wait for writability with the
/// REMAINING deadline via a throwaway poll (mio, already the event-loop
/// substrate). Past the deadline the write fails TimedOut — the caller
/// owns the torn-pipe consequence (a partial line may be buffered).
pub fn write_bounded(
    sender: &mut mio::unix::pipe::Sender,
    bytes: &[u8],
    timeout: Duration,
) -> std::io::Result<()> {
    sender.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    let mut written = 0;
    while written < bytes.len() {
        match sender.write(&bytes[written..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    ErrorKind::WriteZero,
                    "worker stdin accepted zero bytes",
                ));
            }
            Ok(n) => written += n,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(std::io::Error::new(
                        ErrorKind::TimedOut,
                        format!(
                            "worker stdin stayed full past the {}s write deadline \
                             (the worker is not reading its pipe)",
                            timeout.as_secs()
                        ),
                    ));
                }
                // Ok on writable OR on a spurious/timeout wake: the loop
                // re-tries the write and re-checks the deadline, so a
                // spurious wake costs one extra syscall, never a wedge.
                wait_writable(sender, deadline - now)?;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    sender.flush()
}

/// Wait (bounded) for the sender to become writable using a throwaway
/// poll. Spurious, empty, or SIGNAL-INTERRUPTED wakes return Ok — the
/// caller's deadline check governs termination. The Interrupted arm is
/// load-bearing (review residual on PR #51): mio's Poll::poll returns
/// EINTR without retrying, and campd's SA_RESTART SIGCHLD handler never
/// restarts poll/kevent (signal(7)) — so any worker exiting during this
/// ≤2s wait would otherwise fail the nudge and cost a HEALTHY worker its
/// held pipe (torn line + unearned EOF).
fn wait_writable(sender: &mut mio::unix::pipe::Sender, timeout: Duration) -> std::io::Result<()> {
    let mut poll = mio::Poll::new()?;
    let mut events = mio::Events::with_capacity(4);
    poll.registry()
        .register(sender, mio::Token(0), mio::Interest::WRITABLE)?;
    let waited = match poll.poll(&mut events, Some(timeout)) {
        Err(e) if e.kind() == ErrorKind::Interrupted => Ok(()),
        result => result,
    };
    let deregistered = poll.registry().deregister(sender);
    waited?;
    deregistered?;
    Ok(())
}

/// Run a subprocess to completion with a deadline — the bounded
/// `Command::output()`. stdin is null; stdout/stderr are drained through
/// non-blocking pipes with a throwaway poll (the `wait_writable` shape).
/// Past the deadline the child is SIGKILLed, reaped, and the call fails
/// TimedOut naming the program and the bound. On EOF of both pipes the
/// child is reaped with `wait()`: the kernel closes a process's fds AT
/// exit, so EOF-on-both means the child is at (or microseconds from)
/// exit — a process that deliberately closes its own stdout+stderr and
/// then hangs would block that reap, but none of the plumbing campd runs
/// (git, ps, pgrep, kill) does that, and the deadline still bounds every
/// hang that holds its fds, the only shape observed in the wild.
pub fn output_bounded(cmd: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let deadline = Instant::now() + timeout;
    let stdout = child.stdout.take().map(mio::unix::pipe::Receiver::from);
    let stderr = child.stderr.take().map(mio::unix::pipe::Receiver::from);
    let (Some(mut stdout), Some(mut stderr)) = (stdout, stderr) else {
        // Unreachable by construction (both were piped above); still an
        // error, never a panic (invariant 5).
        reap_regardless(&mut child);
        return Err(std::io::Error::other("child pipes unavailable after spawn"));
    };
    match read_both_bounded(&mut stdout, &mut stderr, deadline) {
        Ok((out, err)) => {
            let status = child.wait()?;
            Ok(Output {
                status,
                stdout: out,
                stderr: err,
            })
        }
        Err(e) if e.kind() == ErrorKind::TimedOut => {
            // SIGKILL is unblockable; wait() then reaps promptly (the one
            // unboundable residue is a kernel-side uninterruptible-sleep
            // child, which nothing in userspace can bound).
            child.kill()?;
            child.wait()?;
            Err(std::io::Error::new(
                ErrorKind::TimedOut,
                format!(
                    "{} did not finish within {:?} and was killed (campd bounds every \
                     subprocess it runs on the event loop)",
                    cmd.get_program().to_string_lossy(),
                    timeout
                ),
            ))
        }
        Err(e) => {
            // The read error is the failure being reported; the cleanup
            // may race the child's own exit, so its result cannot be
            // allowed to mask `e`.
            reap_regardless(&mut child);
            Err(e)
        }
    }
}

/// Kill-and-reap where a refusal only means the child already exited.
fn reap_regardless(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Drain both pipes until BOTH reach EOF or the deadline passes
/// (TimedOut). EINTR on the poll re-checks the deadline (the
/// `wait_writable` law: campd's SIGCHLD handler interrupts kevent).
fn read_both_bounded(
    stdout: &mut mio::unix::pipe::Receiver,
    stderr: &mut mio::unix::pipe::Receiver,
    deadline: Instant,
) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    const OUT: mio::Token = mio::Token(0);
    const ERR: mio::Token = mio::Token(1);
    stdout.set_nonblocking(true)?;
    stderr.set_nonblocking(true)?;
    let mut poll = mio::Poll::new()?;
    poll.registry()
        .register(stdout, OUT, mio::Interest::READABLE)?;
    poll.registry()
        .register(stderr, ERR, mio::Interest::READABLE)?;
    let mut events = mio::Events::with_capacity(4);
    let (mut out_buf, mut err_buf) = (Vec::new(), Vec::new());
    let (mut out_open, mut err_open) = (true, true);
    while out_open || err_open {
        let now = Instant::now();
        if now >= deadline {
            return Err(std::io::Error::new(
                ErrorKind::TimedOut,
                "subprocess pipe deadline passed",
            ));
        }
        match poll.poll(&mut events, Some(deadline - now)) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
        for event in events.iter() {
            let token = event.token();
            if token == OUT && out_open && !drain(stdout, &mut out_buf)? {
                out_open = false;
                poll.registry().deregister(stdout)?;
            } else if token == ERR && err_open && !drain(stderr, &mut err_buf)? {
                err_open = false;
                poll.registry().deregister(stderr)?;
            }
        }
    }
    Ok((out_buf, err_buf))
}

/// Read one ready pipe to WouldBlock (still open: true) or EOF (false) —
/// mio is edge-triggered, so every readable event must be drained fully.
fn drain(pipe: &mut mio::unix::pipe::Receiver, buf: &mut Vec<u8>) -> std::io::Result<bool> {
    let mut chunk = [0u8; 4096];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(false),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(true),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// The happy path is byte-identical to `Command::output()`.
    #[test]
    fn output_bounded_returns_a_prompt_commands_output() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let out = output_bounded(
            Command::new("/bin/sh").args(["-c", "printf out; printf err >&2"]),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, b"err");
    }

    /// Output larger than one read chunk (and the pipe buffer) drains
    /// fully — the edge-triggered read must loop to EOF, not stop at the
    /// first chunk.
    #[test]
    fn output_bounded_drains_multi_chunk_output() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let out = output_bounded(
            Command::new("/bin/sh").args(["-c", "head -c 200000 /dev/zero"]),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 200_000);
    }

    /// The wedge shape (issue #55): a subprocess that never exits is
    /// killed at the deadline and surfaces as TimedOut naming the bound —
    /// campd's event loop stalls for the bound, never forever.
    #[test]
    fn output_bounded_kills_a_hung_command_at_the_deadline() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let start = Instant::now();
        let err = output_bounded(Command::new("sleep").arg("30"), Duration::from_millis(300))
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::TimedOut, "got: {err}");
        assert!(
            err.to_string().contains("did not finish within"),
            "the error must name the bound: {err}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "bounded, not wedged: took {:?}",
            start.elapsed()
        );
    }

    /// Early output must not fool the deadline: EOF (exit), not first
    /// byte, is the completion signal.
    #[test]
    fn output_bounded_is_not_fooled_by_output_before_the_hang() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let start = Instant::now();
        let err = output_bounded(
            Command::new("/bin/sh").args(["-c", "echo early; exec sleep 30"]),
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::TimedOut, "got: {err}");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "bounded, not wedged: took {:?}",
            start.elapsed()
        );
    }

    /// A spawn failure (missing binary) is an error, not a hang or panic.
    #[test]
    fn output_bounded_surfaces_a_missing_binary_as_an_error() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let err = output_bounded(
            &mut Command::new("/nonexistent/no-such-binary"),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound, "got: {err}");
    }

    /// The moved PR #51 mechanism, pinned at its new home: a reader that
    /// never drains fails the write at the deadline, bounded.
    #[test]
    fn write_bounded_fails_at_the_deadline_when_the_reader_never_drains() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut sender = mio::unix::pipe::Sender::from(child.stdin.take().unwrap());
        let payload = vec![b'x'; 2 * 1024 * 1024]; // far past any pipe buffer
        let start = Instant::now();
        let err = write_bounded(&mut sender, &payload, Duration::from_millis(300)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::TimedOut, "got: {err}");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "bounded, not wedged: took {:?}",
            start.elapsed()
        );
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
