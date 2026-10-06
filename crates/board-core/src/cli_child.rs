//! Spawning and killing the model-catalog CLIs (`opencode`, `agy`) so that a
//! kill reaches every process the CLI started, not only the direct child.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

pub(crate) fn spawn_in_own_group(cmd: &mut Command) -> io::Result<Child> {
    cmd.process_group(0).spawn()
}

/// SIGKILL the child's whole process group, then reap the child.
///
/// A grandchild that inherited stdout keeps the pipe open after the direct
/// child dies, so a reader thread joined afterwards would block until that
/// grandchild exits on its own. Killing the group closes every writer.
pub(crate) fn kill_group_and_reap(child: &mut Child) {
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
