use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use notepust_contracts::{
    FinishReason, MatchBatch, PROTOCOL_VERSION, Progress, ReplacementReady, SearchRequest,
    SearchStats, WorkerRequest, WorkerResponse, decode_frame, encode_frame, read_frame,
};
use notepust_platform::ChildProcess;

use crate::error::SearchError;

/// Result of one worker job.
#[derive(Clone, Debug, Default)]
pub struct WorkerRun {
    pub stats: SearchStats,
    pub replacement: Option<ReplacementReady>,
    pub batches: u32,
}

/// A persistent, supervised `search-worker` process.
///
/// One client runs at most one job. A cancelled or timed-out job kills and
/// reaps the process; the supervisor never treats a dropped future as proof
/// that regex work stopped.
pub struct WorkerClient {
    process: Option<ChildProcess>,
    stdin: Option<std::process::ChildStdin>,
    frames: Receiver<Result<Vec<u8>, String>>,
    build_id: String,
    busy: bool,
}

impl WorkerClient {
    pub fn spawn(worker_path: &std::path::Path) -> Result<Self, SearchError> {
        let mut command = Command::new(worker_path);
        let mut process = ChildProcess::spawn("search-worker", &mut command).map_err(|error| {
            SearchError::Protocol {
                detail: error.to_string(),
            }
        })?;
        let stdin = process.take_stdin();
        let stdout = process.take_stdout();
        let stderr = process.take_stderr();
        if let Some(stderr) = stderr {
            std::thread::spawn(move || {
                use std::io::BufRead;
                let reader = std::io::BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    tracing::debug!(target: "search-worker", "{line}");
                }
            });
        }
        let stdout = stdout.ok_or_else(|| SearchError::Protocol {
            detail: "worker stdout was not captured".to_string(),
        })?;
        let (tx, frames) = channel();
        std::thread::Builder::new()
            .name("search-worker-reader".to_string())
            .spawn(move || {
                let mut reader = std::io::BufReader::new(stdout);
                loop {
                    match read_frame(&mut reader) {
                        Ok(Some(frame)) => {
                            if tx.send(Ok(frame)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = tx.send(Err(error.to_string()));
                            break;
                        }
                    }
                }
            })
            .map_err(|error| SearchError::Protocol {
                detail: format!("could not start worker reader: {error}"),
            })?;

        let mut client = Self {
            process: Some(process),
            stdin,
            frames,
            build_id: String::new(),
            busy: false,
        };
        client.handshake()?;
        Ok(client)
    }

    pub fn build_id(&self) -> &str {
        &self.build_id
    }

    fn handshake(&mut self) -> Result<(), SearchError> {
        self.send(&WorkerRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
            build_id: env!("CARGO_PKG_VERSION").to_string(),
        })?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.kill();
                return Err(SearchError::Protocol {
                    detail: "worker did not complete the handshake".to_string(),
                });
            }
            let frame = match self.frames.recv_timeout(remaining) {
                Ok(Ok(frame)) => frame,
                Ok(Err(detail)) => {
                    self.kill();
                    return Err(SearchError::Protocol { detail });
                }
                Err(_) => {
                    self.kill();
                    return Err(SearchError::Protocol {
                        detail: "worker handshake timed out".to_string(),
                    });
                }
            };
            let response: WorkerResponse =
                decode_frame(&frame).map_err(|error| SearchError::Protocol {
                    detail: error.to_string(),
                })?;
            match response {
                WorkerResponse::Ready {
                    protocol_version,
                    build_id,
                    engines,
                } => {
                    if protocol_version != PROTOCOL_VERSION {
                        self.kill();
                        return Err(SearchError::Protocol {
                            detail: format!(
                                "worker protocol {protocol_version} does not match {PROTOCOL_VERSION}"
                            ),
                        });
                    }
                    if !engines.contains(&notepust_contracts::RegexEngine::Standard) {
                        self.kill();
                        return Err(SearchError::Protocol {
                            detail: "worker does not support the standard regex engine".to_string(),
                        });
                    }
                    self.build_id = build_id;
                    return Ok(());
                }
                WorkerResponse::Failed { detail, .. } => {
                    self.kill();
                    return Err(SearchError::Protocol { detail });
                }
                _ => {}
            }
        }
    }

    fn send(&mut self, request: &WorkerRequest) -> Result<(), SearchError> {
        let stdin = self.stdin.as_mut().ok_or_else(|| SearchError::Protocol {
            detail: "worker stdin is closed".to_string(),
        })?;
        let frame = encode_frame(request).map_err(|error| SearchError::Protocol {
            detail: error.to_string(),
        })?;
        stdin
            .write_all(&frame)
            .and_then(|_| stdin.flush())
            .map_err(|error| {
                self.kill();
                SearchError::Protocol {
                    detail: format!("could not send to worker: {error}"),
                }
            })
    }

    /// Runs one search. The callbacks are invoked on the calling thread and
    /// must not block the UI.
    pub fn search(
        &mut self,
        request: SearchRequest,
        cancel: &AtomicBool,
        mut on_batch: impl FnMut(MatchBatch),
        mut on_progress: impl FnMut(Progress),
    ) -> Result<WorkerRun, SearchError> {
        if self.busy {
            return Err(SearchError::Protocol {
                detail: "this worker already has a running job".to_string(),
            });
        }
        self.busy = true;
        let result = self.search_inner(request, cancel, &mut on_batch, &mut on_progress);
        self.busy = false;
        result
    }

    fn search_inner(
        &mut self,
        request: SearchRequest,
        cancel: &AtomicBool,
        on_batch: &mut impl FnMut(MatchBatch),
        on_progress: &mut impl FnMut(Progress),
    ) -> Result<WorkerRun, SearchError> {
        let timeout = Duration::from_millis(request.timeout_ms.clamp(1, 30_000));
        let deadline = Instant::now() + timeout + Duration::from_secs(2);
        self.send(&WorkerRequest::Search(request))?;

        let mut run = WorkerRun::default();
        loop {
            if cancel.load(Ordering::Relaxed) {
                self.kill();
                return Err(SearchError::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                self.kill();
                return Err(SearchError::ResourceLimit {
                    detail: "search worker exceeded its deadline and was terminated".to_string(),
                });
            }
            let wait = (deadline - now).min(Duration::from_millis(100));
            let frame = match self.frames.recv_timeout(wait) {
                Ok(Ok(frame)) => frame,
                Ok(Err(detail)) => {
                    self.kill();
                    return Err(SearchError::Protocol { detail });
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.kill();
                    return Err(SearchError::Protocol {
                        detail: "worker output channel closed".to_string(),
                    });
                }
            };
            let response: WorkerResponse =
                decode_frame(&frame).map_err(|error| SearchError::Protocol {
                    detail: error.to_string(),
                })?;
            match response {
                WorkerResponse::MatchBatch(batch) => {
                    run.batches = run.batches.saturating_add(1);
                    on_batch(batch);
                }
                WorkerResponse::Progress(progress) => on_progress(progress),
                WorkerResponse::ReplacementReady(replacement) => {
                    run.replacement = Some(replacement);
                }
                WorkerResponse::Finished {
                    reason,
                    stats,
                    job_id: _,
                } => {
                    run.stats = stats;
                    return match reason {
                        FinishReason::Complete | FinishReason::Truncated => Ok(run),
                        FinishReason::Cancelled => Err(SearchError::Cancelled),
                        FinishReason::Partial => Err(SearchError::Protocol {
                            detail: "worker reported a partial result".to_string(),
                        }),
                        FinishReason::Failed => Err(SearchError::Protocol {
                            detail: "worker reported failure".to_string(),
                        }),
                    };
                }
                WorkerResponse::Failed {
                    category, detail, ..
                } => {
                    return Err(match category {
                        notepust_contracts::ErrorCategory::Cancelled => SearchError::Cancelled,
                        notepust_contracts::ErrorCategory::ResourceLimit => {
                            SearchError::ResourceLimit { detail }
                        }
                        other => SearchError::Protocol {
                            detail: format!("{}: {detail}", other.as_str()),
                        },
                    });
                }
                WorkerResponse::Ready { .. } => {
                    self.kill();
                    return Err(SearchError::Protocol {
                        detail: "unexpected Ready frame".to_string(),
                    });
                }
            }
        }
    }

    fn kill(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.shutdown();
        }
        self.stdin = None;
    }

    pub fn shutdown(&mut self) {
        if !self.busy && self.stdin.is_some() {
            let _ = self.send(&WorkerRequest::Shutdown);
        }
        self.kill();
    }
}

impl Drop for WorkerClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notepust_contracts::{
        JobId, NativePath, RegexEngine, SearchOptions, SearchPurpose, SnapshotId,
    };

    fn worker_binary() -> Option<std::path::PathBuf> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/search-worker.exe");
        path.exists().then_some(path)
    }

    #[test]
    fn supervisor_runs_a_job_when_the_worker_binary_exists() {
        let Some(path) = worker_binary() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("snapshot.txt");
        std::fs::write(&snapshot, "one two two").unwrap();
        let mut client = WorkerClient::spawn(&path).unwrap();
        let request = SearchRequest {
            job_id: JobId::new(),
            purpose: SearchPurpose::Find,
            engine: RegexEngine::Standard,
            pattern: "two".to_string(),
            options: SearchOptions {
                case_sensitive: true,
                ..Default::default()
            },
            snapshot_id: SnapshotId::new(),
            path: NativePath::from_path(&snapshot),
            utf8_length: 11,
            timeout_ms: 5000,
            result_limit: 100,
            replacement: None,
            output_artifact_path: None,
        };
        let run = client
            .search(request, &AtomicBool::new(false), |_| {}, |_| {})
            .unwrap();
        assert_eq!(run.stats.matches_found, 2);
        assert_eq!(run.batches, 1);
    }
}
