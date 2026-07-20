// SPDX-License-Identifier: GPL-3.0-or-later

//! Checked Unix signal/termination helpers for spawned children.
//!
//! Rust's `Child` can only SIGKILL, so `gpu-screen-recorder` control signals
//! (WR-006) and FFmpeg termination (WR-007) go through `libc::kill` here.

use std::io;
use std::process::Child;
use std::time::{Duration, Instant};

/// The runtime SIGRTMIN value; GSR toggles regular recording with it.
pub fn sigrtmin() -> i32 {
    libc::SIGRTMIN()
}

/// Send `signal` to a live child. Rejects a missing/zero PID and converts the
/// OS failure into the underlying error.
pub fn send_signal(child: &Child, signal: i32) -> io::Result<()> {
    let pid = child.id();
    if pid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "child has no pid",
        ));
    }
    let pid = i32::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid out of range"))?;
    if unsafe { libc::kill(pid, signal) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Wait for the child to exit within `timeout`, polling `try_wait`. Returns
/// true when it exited (and was reaped), false on timeout.
pub fn wait_with_timeout(child: &mut Child, timeout: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Graceful stop: SIGINT, wait up to `grace`, then SIGKILL and reap.
pub fn terminate(child: &mut Child, grace: Duration) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    let _ = send_signal(child, libc::SIGINT);
    if wait_with_timeout(child, grace)? {
        return Ok(());
    }
    child.kill()?;
    child.wait()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn signal_and_terminate_control_a_real_child() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        // A benign signal succeeds against a live child.
        send_signal(&child, 0).expect("signal 0");
        terminate(&mut child, Duration::from_millis(500)).expect("terminate");
        // The child is reaped: signalling now fails.
        assert!(send_signal(&child, 0).is_err() || child.try_wait().unwrap().is_some());
    }
}
