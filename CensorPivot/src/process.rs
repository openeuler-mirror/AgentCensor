//! Bounded, deadline-driven child I/O. No reader threads can outlive a transaction.
use crate::{PivotError, Result};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub(crate) struct Process {
    child: Child,
    input: Option<ChildStdin>,
    output: Option<ChildStdout>,
    error: Option<ChildStderr>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    limit: usize,
    truncated: bool,
    status: Option<ExitStatus>,
    // A mounter supervisor needs SIGTERM to run cgroup.kill before exiting.
    supervised: bool,
}

pub(crate) struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
    pub timed_out: bool,
}

impl Process {
    pub fn spawn(command: &mut Command, limit: usize, supervised: bool) -> Result<Self> {
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        let mut process = Self {
            child,
            input: None,
            output: None,
            error: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            limit,
            truncated: false,
            status: None,
            supervised,
        };
        process.input = process.child.stdin.take();
        process.output = process.child.stdout.take();
        process.error = process.child.stderr.take();
        if let Some(fd) = &process.input {
            nonblocking(fd)?;
        }
        if let Some(fd) = &process.output {
            nonblocking(fd)?;
        }
        if let Some(fd) = &process.error {
            nonblocking(fd)?;
        }
        Ok(process)
    }

    pub fn close_input(&mut self) {
        self.input.take();
    }

    pub fn send(&mut self, bytes: &[u8], timeout: Duration) -> Result<()> {
        let started = Instant::now();
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let input = self
                .input
                .as_mut()
                .ok_or_else(|| PivotError::Protocol("runner input closed".into()))?;
            match input.write(remaining) {
                Ok(0) => return Err(PivotError::Protocol("runner input closed".into())),
                Ok(n) => remaining = &remaining[n..],
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
            self.pump()?;
            if started.elapsed() >= timeout {
                return Err(self.failure("runner write timeout"));
            }
            if !remaining.is_empty() {
                pause();
            }
        }
        Ok(())
    }

    pub fn line(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let started = Instant::now();
        loop {
            self.pump()?;
            if self.truncated {
                return Err(self.failure("runner output exceeded limit"));
            }
            if let Some(end) = self.stdout.iter().position(|byte| *byte == b'\n') {
                return Ok(self.stdout.drain(..=end).collect());
            }
            if self.output.is_none() {
                return Err(self.failure("runner exited before reply"));
            }
            if started.elapsed() >= timeout {
                return Err(self.failure("runner reply timeout"));
            }
            pause();
        }
    }

    pub fn collect(mut self, timeout: Duration) -> Result<Output> {
        self.close_input();
        let started = Instant::now();
        let timed_out = loop {
            self.pump()?;
            if self.status.is_none() {
                self.status = self.child.try_wait()?;
            }
            if self.status.is_some() && self.output.is_none() && self.error.is_none() {
                break false;
            }
            // Descendants retaining pipe FDs are subject to the same deadline as the tool.
            if started.elapsed() >= timeout {
                self.stop();
                break true;
            }
            pause();
        };
        // Clean remaining members of ordinary tool/CLI process groups on normal exit too.
        if !self.supervised {
            self.signal(Signal::SIGKILL);
        }
        self.pump()?;
        let status = self
            .status
            .ok_or_else(|| self.failure("child did not exit"))?;
        Ok(Output {
            status,
            stdout: std::mem::take(&mut self.stdout),
            stderr: std::mem::take(&mut self.stderr),
            truncated: self.truncated,
            timed_out,
        })
    }

    fn pump(&mut self) -> Result<()> {
        self.truncated |= drain(&mut self.output, &mut self.stdout, self.limit)?;
        self.truncated |= drain(&mut self.error, &mut self.stderr, self.limit)?;
        Ok(())
    }

    fn failure(&self, message: &str) -> PivotError {
        PivotError::component(
            "process",
            format!(
                "{message}: {}",
                String::from_utf8_lossy(&self.stderr).trim()
            ),
        )
    }

    fn signal(&self, signal: Signal) {
        if let Ok(pid) = i32::try_from(self.child.id()) {
            let _ = killpg(Pid::from_raw(pid), signal);
        }
    }

    fn stop(&mut self) {
        self.close_input();
        if self.supervised {
            self.signal(Signal::SIGTERM);
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(6) {
                if let Ok(Some(status)) = self.child.try_wait() {
                    self.status = Some(status);
                    return;
                }
                let _ = self.pump();
                pause();
            }
        }
        self.signal(Signal::SIGKILL);
        let _ = self.child.kill();
        if let Ok(status) = self.child.wait() {
            self.status = Some(status);
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.status.is_none() {
            self.stop();
        }
    }
}

fn nonblocking(fd: &impl AsFd) -> Result<()> {
    let flags = fcntl(fd, FcntlArg::F_GETFL).map_err(std::io::Error::from)?;
    fcntl(
        fd,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
    )
    .map_err(std::io::Error::from)?;
    Ok(())
}

fn drain(
    reader: &mut Option<impl Read>,
    kept: &mut Vec<u8>,
    limit: usize,
) -> std::io::Result<bool> {
    let Some(stream) = reader else {
        return Ok(false);
    };
    let mut truncated = false;
    let mut buffer = [0; 8192];
    // Budget each poll so an infinite writer cannot starve timeout checks.
    for _ in 0..16 {
        match stream.read(&mut buffer) {
            Ok(0) => {
                *reader = None;
                break;
            }
            Ok(n) => {
                let available = limit.saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..n.min(available)]);
                truncated |= n > available;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(truncated)
}

fn pause() {
    std::thread::sleep(Duration::from_millis(5));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infinite_output_is_bounded_and_still_times_out() -> Result<()> {
        let output = Process::spawn(
            Command::new("/bin/sh").args(["-c", "while :; do printf 0123456789; done"]),
            128,
            false,
        )?
        .collect(Duration::from_millis(100))?;
        assert!(output.timed_out && output.truncated);
        assert_eq!(output.stdout.len(), 128);
        Ok(())
    }

    #[test]
    fn inherited_pipe_does_not_block_after_parent_exit() -> Result<()> {
        let started = Instant::now();
        let output = Process::spawn(
            Command::new("/bin/sh").args(["-c", "sleep 30 & exit 0"]),
            128,
            false,
        )?
        .collect(Duration::from_millis(100))?;
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(3));
        Ok(())
    }

    #[test]
    fn missing_ready_and_oversized_frames_fail_without_hanging() -> Result<()> {
        let mut silent = Process::spawn(Command::new("/bin/sleep").arg("30"), 32, false)?;
        assert!(silent.line(Duration::from_millis(50)).is_err());
        let mut flood = Process::spawn(
            Command::new("/bin/sh").args(["-c", "while :; do printf x; done"]),
            32,
            false,
        )?;
        assert!(flood.line(Duration::from_secs(1)).is_err());
        Ok(())
    }
}
