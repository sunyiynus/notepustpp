use std::process::{Child, Command, Stdio};

/// Error from worker process supervision.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("failed to start process: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("failed to terminate process: {0}")]
    Terminate(#[source] std::io::Error),
    #[error("failed to wait for process: {0}")]
    Wait(#[source] std::io::Error),
}

/// A spawned worker with deterministic teardown.
///
/// `std::process::Child` does not terminate on drop. This wrapper always kills
/// and reaps on explicit shutdown or drop, so a cancelled search cannot leave a
/// runaway regex process behind. A Windows Job Object will be added around this
/// wrapper when the worker supervisor is hardened; ordinary child teardown
/// already provides the required kill + wait behaviour for version one.
#[derive(Debug)]
pub struct ChildProcess {
    child: Option<Child>,
    description: String,
}

impl ChildProcess {
    pub fn spawn(
        description: impl Into<String>,
        command: &mut Command,
    ) -> Result<Self, ProcessError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: the worker must never flash a console.
            command.creation_flags(0x0800_0000);
        }
        let child = command.spawn().map_err(ProcessError::Spawn)?;
        Ok(Self {
            child: Some(child),
            description: description.into(),
        })
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn child_mut(&mut self) -> Option<&mut Child> {
        self.child.as_mut()
    }

    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.as_mut().and_then(|child| child.stdin.take())
    }

    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.as_mut().and_then(|child| child.stdout.take())
    }

    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.child.as_mut().and_then(|child| child.stderr.take())
    }

    pub fn id(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self.child.as_mut() {
            Some(child) => child.try_wait(),
            None => Ok(None),
        }
    }

    /// Kills the process if it is still running and reaps it.
    pub fn shutdown(&mut self) -> Result<(), ProcessError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) => return Err(ProcessError::Wait(error)),
        }
        let kill_result = child.kill();
        let wait_result = child.wait();
        if let Err(error) = wait_result {
            return Err(ProcessError::Wait(error));
        }
        match kill_result {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(ProcessError::Terminate(error)),
        }
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn spawn_and_reap_a_short_lived_process() {
        let mut command = Command::new("cmd");
        command.args(["/C", "exit 0"]);
        let mut process = ChildProcess::spawn("test", &mut command).unwrap();
        let status = process.child_mut().unwrap().wait().unwrap();
        assert!(status.success());
    }
}
