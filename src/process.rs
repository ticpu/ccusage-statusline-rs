use crate::warn;
use std::io;
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Debug)]
pub enum RunError {
    Spawn(io::Error),
    TimedOut,
    Wait(io::Error),
}

/// Run a command to completion with piped output, killing it past `timeout`.
/// Output must stay small: pipes are drained only after the child exits.
pub fn run_bounded(cmd: &mut Command, timeout: Duration) -> Result<Output, RunError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Spawn)?;

    match child.wait_timeout(timeout) {
        Ok(Some(_)) => child
            .wait_with_output()
            .map_err(RunError::Wait),
        Ok(None) => {
            let program = cmd
                .get_program()
                .to_string_lossy();
            if let Err(e) = child.kill() {
                warn!("{program} timed out and could not be killed, leaving it running: {e}");
            } else if let Err(e) = child.wait() {
                warn!("{program} timed out, killed but not reaped: {e}");
            }
            Err(RunError::TimedOut)
        }
        Err(e) => Err(RunError::Wait(e)),
    }
}
