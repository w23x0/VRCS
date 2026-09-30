//! Launching helper processes that hand a path or URL to the desktop.
//!
//! `std::process::Command::spawn` hands back a `Child` that has to be `wait`ed for. Dropping it
//! closes the process handle but never collects the exit status, so on Unix the helper stays in
//! the process table as a `<defunct>` zombie until the parent exits. Both callers of this module
//! (`open_directory`, `open_vrcx_repository`) launch a one-shot helper and ignore its output, so
//! waiting would be pure latency on the UI thread — but somebody has to wait, and that somebody is
//! a background thread.

use std::process::Command;

/// Spawns `command` and reaps it from a background thread.
///
/// Nothing is read from the child and its exit status is only logged, so callers see the same
/// result as a bare `spawn`; the difference is that the child does not outlive us as a zombie.
/// A failure to start the reaper thread is not fatal either: the `Child` is dropped, which is the
/// pre-existing behavior.
pub(crate) fn spawn_detached(command: &mut Command) -> std::io::Result<()> {
    spawn_reaped(command).map(|_| ())
}

fn spawn_reaped(command: &mut Command) -> std::io::Result<u32> {
    let mut child = command.spawn()?;
    let pid = child.id();

    std::thread::Builder::new()
        .name("vrcs-helper-reaper".to_string())
        .spawn(move || {
            if let Err(error) = child.wait() {
                tracing::debug!(%error, "launched helper process could not be reaped");
            }
        })
        .map(|_| pid)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::spawn_reaped;

    /// Reads the process state (the third field of `/proc/<pid>/stat`) of a live pid.
    ///
    /// `comm` is parenthesized and may itself contain spaces and parentheses, so the fields are
    /// taken from behind the last `") "`, not from the front of the line.
    fn process_state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (_, after_comm) = stat.rsplit_once(") ")?;
        after_comm.split_whitespace().next()?.chars().next()
    }

    /// Polls `pid` every 10ms until `done` accepts its state, or the timeout expires.
    fn poll_until(pid: u32, mut done: impl FnMut(Option<char>) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if done(process_state(pid)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The observation the fix rests on: a child that is never `wait`ed for really does sit in the
    /// process table as a zombie, so the assertion below is not vacuous.
    #[test]
    #[allow(clippy::zombie_processes)] // the un-reaped child is the point of this test
    fn child_spawned_without_waiting_stays_a_zombie() {
        let child = Command::new("true").spawn().expect("`true` should start");
        let pid = child.id();

        assert!(
            poll_until(pid, |state| state == Some('Z')),
            "expected pid {pid} to become a zombie, last state {:?}",
            process_state(pid)
        );
    }

    #[test]
    fn detached_helper_is_reaped() {
        let pid = spawn_reaped(&mut Command::new("true")).expect("`true` should start");

        // The strong form of "not a zombie": the pid leaves `/proc` altogether, so the reaper
        // really collected it rather than the test merely catching it between states.
        assert!(
            poll_until(pid, |state| state.is_none()),
            "pid {pid} was never reaped, last state {:?}",
            process_state(pid)
        );
    }
}
