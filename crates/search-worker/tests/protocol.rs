use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use notepust_contracts::{
    FinishReason, JobId, NativePath, PROTOCOL_VERSION, RegexEngine, ReplacementPiece,
    ReplacementProgram, SearchOptions, SearchPurpose, SearchRequest, SnapshotId, WorkerRequest,
    WorkerResponse, decode_frame, encode_frame,
};

struct Worker {
    child: Child,
    stdin: ChildStdin,
    frames: Receiver<Result<Vec<u8>, String>>,
}

impl Worker {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_search-worker"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn search-worker");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, frames) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match notepust_contracts::read_frame(&mut reader) {
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
        });
        let mut worker = Self {
            child,
            stdin,
            frames,
        };
        worker.send(&WorkerRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
            build_id: "test".to_string(),
        });
        match worker.recv() {
            WorkerResponse::Ready { engines, .. } => {
                assert!(engines.contains(&RegexEngine::Standard));
                assert!(engines.contains(&RegexEngine::Advanced));
            }
            other => panic!("expected Ready, got {other:?}"),
        }
        worker
    }

    fn send(&mut self, request: &WorkerRequest) {
        let frame = encode_frame(request).unwrap();
        self.stdin.write_all(&frame).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self) -> WorkerResponse {
        let frame = self
            .frames
            .recv_timeout(Duration::from_secs(10))
            .expect("worker did not answer")
            .expect("worker protocol read failed");
        decode_frame(&frame).expect("valid worker frame")
    }

    fn request(
        path: &std::path::Path,
        pattern: &str,
        engine: RegexEngine,
        purpose: SearchPurpose,
        replacement: Option<ReplacementProgram>,
        artifact: Option<&std::path::Path>,
    ) -> SearchRequest {
        let bytes = std::fs::read(path).unwrap();
        SearchRequest {
            job_id: JobId::new(),
            purpose,
            engine,
            pattern: pattern.to_string(),
            options: SearchOptions {
                case_sensitive: true,
                ..Default::default()
            },
            snapshot_id: SnapshotId::new(),
            path: NativePath::from_path(path),
            utf8_length: bytes.len() as u64,
            timeout_ms: 5_000,
            result_limit: 100,
            replacement,
            output_artifact_path: artifact.map(NativePath::from_path),
        }
    }

    fn finish(mut self) {
        self.send(&WorkerRequest::Shutdown);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn standard_search_and_advanced_lookaround() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = dir.path().join("snapshot.txt");
    std::fs::write(&snapshot, "hello world\nhello there\n").unwrap();

    let mut worker = Worker::start();
    let standard = Worker::request(
        &snapshot,
        "hello",
        RegexEngine::Standard,
        SearchPurpose::Find,
        None,
        None,
    );
    worker.send(&WorkerRequest::Search(standard));
    let mut matches = 0usize;
    loop {
        match worker.recv() {
            WorkerResponse::MatchBatch(batch) => matches += batch.matches.len(),
            WorkerResponse::Finished { reason, stats, .. } => {
                assert_eq!(reason, FinishReason::Complete);
                assert_eq!(stats.matches_found, 2);
                break;
            }
            WorkerResponse::Progress(_) => {}
            other => panic!("unexpected response {other:?}"),
        }
    }
    assert_eq!(matches, 2);

    let advanced = Worker::request(
        &snapshot,
        "hello(?= world)",
        RegexEngine::Advanced,
        SearchPurpose::Find,
        None,
        None,
    );
    worker.send(&WorkerRequest::Search(advanced));
    loop {
        match worker.recv() {
            WorkerResponse::MatchBatch(batch) => assert_eq!(batch.matches.len(), 1),
            WorkerResponse::Finished { stats, .. } => {
                assert_eq!(stats.matches_found, 1);
                break;
            }
            WorkerResponse::Progress(_) => {}
            other => panic!("unexpected response {other:?}"),
        }
    }
    worker.finish();
}

#[test]
fn replacement_artifact_uses_capture_groups() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = dir.path().join("snapshot.txt");
    std::fs::write(&snapshot, "hello world\nhello there\n").unwrap();
    let artifact = dir.path().join("artifact.txt");

    let program = ReplacementProgram {
        pieces: vec![
            ReplacementPiece::Group { index: 1 },
            ReplacementPiece::Literal {
                text: "!".to_string(),
            },
        ],
    };
    let request = Worker::request(
        &snapshot,
        "(hello)",
        RegexEngine::Standard,
        SearchPurpose::ReplacePlan,
        Some(program),
        Some(&artifact),
    );
    let mut worker = Worker::start();
    worker.send(&WorkerRequest::Search(request));
    let mut ready = None;
    loop {
        match worker.recv() {
            WorkerResponse::MatchBatch(_) | WorkerResponse::Progress(_) => {}
            WorkerResponse::ReplacementReady(result) => ready = Some(result),
            WorkerResponse::Finished { .. } => break,
            other => panic!("unexpected response {other:?}"),
        }
    }
    let ready = ready.expect("replacement result");
    assert_eq!(ready.match_count, 2);
    assert_eq!(ready.utf8_length, 26);
    let output = std::fs::read(&artifact).unwrap();
    assert_eq!(output, b"hello! world\nhello! there\n");
    assert_eq!(*blake3::hash(&output).as_bytes(), ready.digest);
    worker.finish();
}

#[test]
fn invalid_pattern_returns_a_matchable_error() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = dir.path().join("snapshot.txt");
    std::fs::write(&snapshot, "abc").unwrap();
    let request = Worker::request(
        &snapshot,
        "(",
        RegexEngine::Standard,
        SearchPurpose::Find,
        None,
        None,
    );
    let mut worker = Worker::start();
    worker.send(&WorkerRequest::Search(request));
    match worker.recv() {
        WorkerResponse::Failed { category, .. } => {
            assert_eq!(category, notepust_contracts::ErrorCategory::InvalidRequest);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    worker.finish();
}
