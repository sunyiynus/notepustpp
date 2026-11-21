//! Single-instance ownership and command forwarding.
//!
//! The first process creates a local named mutex and serves a named pipe. Later
//! processes detect the mutex, send one JSON request over the pipe and exit.
//! Only paths and an optional line/column are forwarded; no document content
//! crosses the pipe.

use std::sync::mpsc::Receiver;

/// One open request forwarded from a second process.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ForwardedOpen {
    pub paths: Vec<String>,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum SingleInstanceError {
    #[cfg(windows)]
    #[error("single-instance setup failed: {0}")]
    Windows(#[from] windows::core::Error),

    #[error("could not connect to the running Notepust instance: {0}")]
    Connect(String),

    #[error("single-instance forwarding is not supported on this platform")]
    Unsupported,
}

/// What the current process should do.
pub enum Claim {
    /// Serve forwarded requests. The guard must live for the process lifetime.
    Primary {
        receiver: Receiver<ForwardedOpen>,
        guard: InstanceGuard,
    },
    /// The request was sent to the primary and this process should exit.
    Forwarded,
}

pub struct InstanceGuard {
    #[cfg(windows)]
    mutex: windows::Win32::Foundation::HANDLE,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.mutex);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, GENERIC_WRITE,
        GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
        ReadFile, WriteFile,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::{HRESULT, PCWSTR};

    use super::{Claim, ForwardedOpen, InstanceGuard, SingleInstanceError};

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn pipe_path(name: &str) -> String {
        format!(r"\\.\pipe\{name}")
    }

    /// Named-pipe handles are only used by the dedicated server thread.
    struct SendHandle(HANDLE);
    unsafe impl Send for SendHandle {}

    #[allow(unsafe_code)]
    pub fn claim(name: &str, request: &ForwardedOpen) -> Result<Claim, SingleInstanceError> {
        let mutex_name = format!(r"Local\{name}");
        let wide_mutex = wide(&mutex_name);
        let mutex = unsafe { CreateMutexW(None, false, PCWSTR(wide_mutex.as_ptr())) }?;
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already_exists {
            unsafe {
                let _ = CloseHandle(mutex);
            }
            forward(name, request)?;
            return Ok(Claim::Forwarded);
        }

        let pipe_name = pipe_path(name);
        let wide_pipe = wide(&pipe_name);
        let pipe = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide_pipe.as_ptr()),
                PIPE_ACCESS_INBOUND,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                64 * 1024,
                64 * 1024,
                0,
                None,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            let code = unsafe { GetLastError() };
            unsafe {
                let _ = CloseHandle(mutex);
            }
            return Err(SingleInstanceError::Windows(windows::core::Error::new(
                HRESULT(code.0 as i32),
                format!("CreateNamedPipeW failed with Win32 error {}", code.0),
            )));
        }

        let (sender, receiver) = channel();
        let thread_pipe = SendHandle(pipe);
        std::thread::Builder::new()
            .name("notepust-single-instance".to_string())
            .spawn(move || {
                serve(thread_pipe, sender);
            })
            .map_err(|error| SingleInstanceError::Connect(error.to_string()))?;

        Ok(Claim::Primary {
            receiver,
            guard: InstanceGuard { mutex },
        })
    }

    #[allow(unsafe_code)]
    fn serve(handle: SendHandle, sender: std::sync::mpsc::Sender<ForwardedOpen>) {
        let pipe = handle.0;
        loop {
            let connected = unsafe { ConnectNamedPipe(pipe, None) };
            let last_error = unsafe { GetLastError() };
            // ERROR_NO_DATA is returned when the client connected and closed
            // before the server reached ConnectNamedPipe; the written payload
            // is still buffered and readable.
            if connected.is_err()
                && last_error != ERROR_PIPE_CONNECTED
                && last_error != ERROR_NO_DATA
            {
                break;
            }
            let mut payload = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let mut read = 0u32;
                let result = unsafe { ReadFile(pipe, Some(&mut buffer), Some(&mut read), None) };
                if result.is_err() || read == 0 {
                    break;
                }
                payload.extend_from_slice(&buffer[..read as usize]);
                if payload.contains(&b'\n') || payload.len() > 1024 * 1024 {
                    break;
                }
            }
            if let Some(newline) = payload.iter().position(|byte| *byte == b'\n') {
                payload.truncate(newline);
            }
            if let Ok(request) = serde_json::from_slice::<ForwardedOpen>(&payload) {
                let _ = sender.send(request);
            }
            let _ = unsafe { DisconnectNamedPipe(pipe) };
        }
        unsafe {
            let _ = CloseHandle(pipe);
        }
    }

    #[allow(unsafe_code)]
    pub fn forward(name: &str, request: &ForwardedOpen) -> Result<(), SingleInstanceError> {
        let path = pipe_path(name);
        let wide_path = wide(&path);
        let mut payload = serde_json::to_vec(request)
            .map_err(|error| SingleInstanceError::Connect(error.to_string()))?;
        payload.push(b'\n');
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(wide_path.as_ptr()),
                    GENERIC_WRITE.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            };
            match opened {
                Ok(handle) => {
                    let mut written = 0u32;
                    let result =
                        unsafe { WriteFile(handle, Some(&payload), Some(&mut written), None) };
                    unsafe {
                        let _ = CloseHandle(handle);
                    }
                    return match result {
                        Ok(()) if written as usize == payload.len() => Ok(()),
                        Ok(()) => Err(SingleInstanceError::Connect(format!(
                            "only {written} of {} bytes were sent",
                            payload.len()
                        ))),
                        Err(error) => Err(SingleInstanceError::Windows(error)),
                    };
                }
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => {
                    return Err(SingleInstanceError::Windows(error));
                }
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{claim, forward};

#[cfg(not(windows))]
pub fn claim(_name: &str, _request: &ForwardedOpen) -> Result<Claim, SingleInstanceError> {
    let (_sender, receiver) = std::sync::mpsc::channel();
    Ok(Claim::Primary {
        receiver,
        guard: InstanceGuard {},
    })
}

#[cfg(not(windows))]
pub fn forward(_name: &str, _request: &ForwardedOpen) -> Result<(), SingleInstanceError> {
    Err(SingleInstanceError::Unsupported)
}

#[cfg(all(test, windows))]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::{Claim, ForwardedOpen, claim, forward};

    static NEXT_NAME: AtomicU32 = AtomicU32::new(1);

    fn unique_name() -> String {
        format!(
            "NotepustTest-{}-{}",
            std::process::id(),
            NEXT_NAME.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn forwards_one_request_to_the_primary_process() {
        let name = unique_name();
        let Claim::Primary { receiver, guard } = claim(&name, &ForwardedOpen::default()).unwrap()
        else {
            panic!("the first claim must become primary");
        };
        let sent = ForwardedOpen {
            paths: vec![r"C:\tmp\a b.txt".to_string()],
            line: Some(7),
            column: Some(2),
        };
        forward(&name, &sent).unwrap();
        let received = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(received, sent);
        drop(guard);
    }

    #[test]
    fn a_second_claim_sends_its_arguments_and_reports_forwarded() {
        let name = unique_name();
        let Claim::Primary { receiver, guard } = claim(&name, &ForwardedOpen::default()).unwrap()
        else {
            panic!("the first claim must become primary");
        };
        let sent = ForwardedOpen {
            paths: vec![r"D:\logs\中文.log".to_string()],
            line: None,
            column: None,
        };
        match claim(&name, &sent).unwrap() {
            Claim::Forwarded => {}
            Claim::Primary { .. } => panic!("the second claim must forward"),
        }
        let received = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(received, sent);
        drop(guard);
    }
}
