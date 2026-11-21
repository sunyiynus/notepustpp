use std::path::PathBuf;
use std::sync::mpsc::{SyncSender, sync_channel};

use notepust_contracts::{DocRevision, DocumentId, EditBatch, NativePath};

use crate::recovery::{RecoveredDocument, RecoveryDb, SessionDocument};

pub enum StorageTask {
    Checkpoint(Box<RecoveredDocument>),
    AppendDelta {
        document_id: DocumentId,
        base_revision: DocRevision,
        batch: Box<EditBatch>,
    },
    Session(Vec<SessionDocument>),
    Recent {
        path: NativePath,
        display_path: String,
    },
    RemoveRecovery(DocumentId),
    Purge(u64),
    Shutdown,
}

/// Cloneable, bounded sender used by the UI/background producers.
#[derive(Clone)]
pub struct StorageSender {
    tx: SyncSender<StorageTask>,
}

impl StorageSender {
    pub fn try_checkpoint(&self, document: RecoveredDocument) -> bool {
        self.tx
            .try_send(StorageTask::Checkpoint(Box::new(document)))
            .is_ok()
    }

    pub fn try_append_delta(
        &self,
        document_id: DocumentId,
        base_revision: DocRevision,
        batch: EditBatch,
    ) -> bool {
        self.tx
            .try_send(StorageTask::AppendDelta {
                document_id,
                base_revision,
                batch: Box::new(batch),
            })
            .is_ok()
    }

    pub fn try_session(&self, documents: Vec<SessionDocument>) -> bool {
        self.tx.try_send(StorageTask::Session(documents)).is_ok()
    }

    pub fn try_recent(&self, path: NativePath, display_path: String) -> bool {
        self.tx
            .try_send(StorageTask::Recent { path, display_path })
            .is_ok()
    }

    pub fn try_remove_recovery(&self, document_id: DocumentId) -> bool {
        self.tx
            .try_send(StorageTask::RemoveRecovery(document_id))
            .is_ok()
    }

    pub fn try_purge(&self, older_than_days: u64) -> bool {
        self.tx
            .try_send(StorageTask::Purge(older_than_days))
            .is_ok()
    }
}

/// Single writer thread for the recovery database.
///
/// The queue is bounded; producers use `try_*` so a busy disk can never block
/// the UI thread. Dropped checkpoints are acceptable: the next periodic
/// checkpoint contains the full text. Errors are logged, never panicked.
pub struct StorageWorker {
    sender: StorageSender,
    join: Option<std::thread::JoinHandle<()>>,
}

impl StorageWorker {
    pub fn spawn(database_path: PathBuf) -> Result<Self, crate::recovery::StorageError> {
        // Open before spawning so a corrupt/incompatible database is reported
        // to the caller synchronously.
        let db = RecoveryDb::open(&database_path)?;
        let (tx, rx) = sync_channel::<StorageTask>(32);
        let join = std::thread::Builder::new()
            .name("notepust-storage".to_string())
            .spawn(move || {
                let mut db = db;
                while let Ok(task) = rx.recv() {
                    match task {
                        StorageTask::Checkpoint(document) => {
                            if let Err(error) = db.checkpoint(&document) {
                                tracing::error!(%error, "recovery checkpoint failed");
                            }
                        }
                        StorageTask::AppendDelta {
                            document_id,
                            base_revision,
                            batch,
                        } => {
                            if let Err(error) = db.append_delta(document_id, base_revision, &batch)
                            {
                                tracing::error!(%error, "recovery delta failed");
                            }
                        }
                        StorageTask::Session(documents) => {
                            if let Err(error) = db.save_session(&documents) {
                                tracing::error!(%error, "session save failed");
                            }
                        }
                        StorageTask::Recent { path, display_path } => {
                            if let Err(error) = db.touch_recent(&path, &display_path) {
                                tracing::error!(%error, "recent-file update failed");
                            }
                        }
                        StorageTask::RemoveRecovery(document_id) => {
                            if let Err(error) = db.remove_recovery(document_id) {
                                tracing::error!(%error, "recovery removal failed");
                            }
                        }
                        StorageTask::Purge(days) => {
                            if let Err(error) = db.purge_older_than(days) {
                                tracing::error!(%error, "recovery purge failed");
                            }
                        }
                        StorageTask::Shutdown => break,
                    }
                }
            })
            .map_err(crate::recovery::StorageError::Io)?;

        Ok(Self {
            sender: StorageSender { tx },
            join: Some(join),
        })
    }

    pub fn sender(&self) -> StorageSender {
        self.sender.clone()
    }

    pub fn shutdown(mut self) {
        let _ = self.sender.tx.send(StorageTask::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for StorageWorker {
    fn drop(&mut self) {
        let _ = self.sender.tx.try_send(StorageTask::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
