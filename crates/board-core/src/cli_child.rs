//! Spawning and killing the model-catalog CLIs (`opencode`, `agy`) so that a
//! kill reaches every process the CLI started, not only the direct child.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Poll interval while reaping a child whose stdout already reached EOF.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

pub fn spawn_in_own_group(cmd: &mut Command) -> io::Result<Child> {
    cmd.process_group(0).spawn()
}

/// SIGKILL the child's whole process group, then reap the child.
///
/// A grandchild that inherited stdout keeps the pipe open after the direct
/// child dies, so a reader thread joined afterwards would block until that
/// grandchild exits on its own. Killing the group closes every writer.
pub fn kill_group_and_reap(child: &mut Child) {
    // Must run before `wait`: the unreaped child keeps its PID (the group id)
    // reserved, so the signal cannot reach a reused group.
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: killpg has no memory-safety preconditions.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Run `argv` and return its stdout, bounded in both dimensions a
/// pathological CLI could exhaust:
/// - **wall clock**: the child's process group is killed (and the child
///   reaped) when `timeout` passes, so a hung CLI, or a process it started
///   that still holds stdout, cannot block the daemon forever;
/// - **stdout**: a reader thread drains stdout into a buffer capped at
///   `max_bytes` (stdin/stderr are null, so no other pipe can fill and
///   deadlock the child), so an unparsable firehose cannot exhaust memory.
///
/// Returns `None` on spawn failure, timeout, oversized output, a failed
/// read, a non-zero exit, or a lost reader thread.
pub(crate) fn run_bounded(argv: &[String], timeout: Duration, max_bytes: usize) -> Option<Vec<u8>> {
    let mut child = spawn_in_own_group(
        Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut buffer: Vec<u8> = Vec::new();
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buffer.len() + n > max_bytes {
                        let _ = tx.send(None);
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
            }
        }
        let _ = tx.send(Some(buffer));
    });
    let deadline = Instant::now() + timeout;
    let stdout = match rx.recv_timeout(timeout) {
        Ok(Some(buffer)) => buffer,
        // Timeout, oversized output, or read failure: kill the child's process
        // group so every pipe writer dies, reap it, and join the reader. The
        // channel is dropped first so a reader blocked on a final send cannot
        // hang the join.
        Ok(None) | Err(_) => {
            kill_group_and_reap(&mut child);
            drop(rx);
            let _ = reader.join();
            return None;
        }
    };
    // The reader saw EOF, but the process itself may still be exiting (a
    // grandchild could hold the pipe). Reap it within the same deadline;
    // a child that will not exit after EOF is killed like a timeout.
    let status = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => std::thread::sleep(remaining.min(POLL_INTERVAL)),
            Err(_) => break None,
        }
    };
    let status = match status {
        Some(status) => status,
        None => {
            kill_group_and_reap(&mut child);
            return None;
        }
    };
    let _ = reader.join();
    if !status.success() {
        return None;
    }
    Some(stdout)
}
