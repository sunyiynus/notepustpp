use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notepust_contracts::{
    DocRevision, DocumentId, EditBatch, EncodingSpec, NativePath, SourceSnapshotId,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const RECOVERY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("recovery database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("recovery serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("recovery database schema {found} is newer than supported ({supported})")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("recovery data is corrupt: {0}")]
    Corrupt(String),
}

/// A recoverable checkpoint. The text is a full snapshot; deltas between
/// checkpoints are advisory until the next checkpoint is written.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveredDocument {
    pub document_id: DocumentId,
    pub path: Option<NativePath>,
    pub display_path: Option<String>,
    pub lifetime_generation: u64,
    pub revision: DocRevision,
    pub encoding: EncodingSpec,
    pub text: String,
    pub digest: [u8; 32],
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionDocument {
    pub document_id: DocumentId,
    pub path: Option<NativePath>,
    pub display_path: String,
    pub revision: DocRevision,
    pub caret_utf8: u64,
    pub scroll_top_line: u64,
    pub active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentFile {
    pub path: NativePath,
    pub display_path: String,
    pub opened_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SaveJournalEntry {
    pub id: i64,
    pub document_id: DocumentId,
    pub revision: DocRevision,
    pub target: NativePath,
    pub temp: NativePath,
    pub rollback: Option<NativePath>,
    pub output_digest: Option<[u8; 32]>,
    pub state: String,
    pub created_at: u64,
}

/// Connection used by exactly one storage writer thread.
///
/// The type is `Send` but intentionally not shared behind a mutex: all writes
/// go through [`crate::StorageWorker`] in the application.
pub struct RecoveryDb {
    conn: Connection,
}

impl RecoveryDb {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::configure(conn)
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()?;
        Self::configure(conn)
    }

    fn configure(conn: Connection) -> Result<Self, StorageError> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA synchronous=FULL;\nPRAGMA foreign_keys=ON;\nPRAGMA temp_store=MEMORY;",
        )?;
        // WAL is best-effort for in-memory databases.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");

        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<(), StorageError> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or(None);

        if let Some(version) = existing {
            let version: u32 = version.parse().unwrap_or(0);
            if version > RECOVERY_SCHEMA_VERSION {
                return Err(StorageError::UnsupportedSchema {
                    found: version,
                    supported: RECOVERY_SCHEMA_VERSION,
                });
            }
        }

        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS documents (
                document_id TEXT PRIMARY KEY,
                path_json TEXT,
                display_path TEXT,
                lifetime_generation INTEGER NOT NULL,
                revision INTEGER NOT NULL,
                encoding_json TEXT NOT NULL,
                text_blob BLOB NOT NULL,
                digest BLOB NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS source_chunks (
                source_snapshot_id TEXT NOT NULL,
                chunk_index INTEGER NOT NULL,
                data BLOB NOT NULL,
                digest BLOB NOT NULL,
                PRIMARY KEY (source_snapshot_id, chunk_index)
            );

            CREATE TABLE IF NOT EXISTS edit_deltas (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                document_id TEXT NOT NULL,
                base_revision INTEGER NOT NULL,
                revision INTEGER NOT NULL,
                delta_json TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS edit_deltas_document
                ON edit_deltas(document_id, revision);

            CREATE TABLE IF NOT EXISTS save_ops (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                document_id TEXT NOT NULL,
                revision INTEGER NOT NULL,
                target_json TEXT NOT NULL,
                temp_json TEXT NOT NULL,
                rollback_json TEXT,
                output_digest BLOB,
                state TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS session_docs (
                position INTEGER PRIMARY KEY,
                document_id TEXT NOT NULL,
                path_json TEXT,
                display_path TEXT NOT NULL,
                revision INTEGER NOT NULL,
                caret_utf8 INTEGER NOT NULL,
                scroll_top_line INTEGER NOT NULL,
                active INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS recent_files (
                path_json TEXT PRIMARY KEY,
                display_path TEXT NOT NULL,
                opened_at INTEGER NOT NULL
            );
            "#,
        )?;

        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![RECOVERY_SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    pub fn store_source_snapshot(
        &mut self,
        source_id: SourceSnapshotId,
        chunks: &[Vec<u8>],
    ) -> Result<[u8; 32], StorageError> {
        let mut hasher = blake3::Hasher::new();
        let transaction = self.conn.transaction()?;
        {
            let mut insert = transaction.prepare(
                "INSERT OR REPLACE INTO source_chunks
                 (source_snapshot_id, chunk_index, data, digest)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (index, chunk) in chunks.iter().enumerate() {
                hasher.update(chunk);
                let digest = *blake3::hash(chunk).as_bytes();
                insert.execute(params![
                    source_id.to_string(),
                    index as i64,
                    chunk,
                    digest.as_slice()
                ])?;
            }
        }
        transaction.commit()?;
        Ok(*hasher.finalize().as_bytes())
    }

    pub fn load_source_snapshot(
        &self,
        source_id: SourceSnapshotId,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT data, digest FROM source_chunks
             WHERE source_snapshot_id = ?1 ORDER BY chunk_index",
        )?;
        let mut rows = statement.query(params![source_id.to_string()])?;
        let mut chunks = Vec::new();
        while let Some(row) = rows.next()? {
            let data: Vec<u8> = row.get(0)?;
            let digest: Vec<u8> = row.get(1)?;
            if digest.as_slice() != blake3::hash(&data).as_bytes() {
                return Err(StorageError::Corrupt(format!(
                    "source chunk {source_id} failed its digest check"
                )));
            }
            chunks.push(data);
        }
        Ok(chunks)
    }

    pub fn delete_source_snapshot(
        &mut self,
        source_id: SourceSnapshotId,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "DELETE FROM source_chunks WHERE source_snapshot_id = ?1",
            params![source_id.to_string()],
        )?;
        Ok(())
    }

    pub fn checkpoint(&mut self, document: &RecoveredDocument) -> Result<(), StorageError> {
        let digest = *blake3::hash(document.text.as_bytes()).as_bytes();
        let transaction = self.conn.transaction()?;
        transaction.execute(
            "INSERT INTO documents
             (document_id, path_json, display_path, lifetime_generation, revision,
              encoding_json, text_blob, digest, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(document_id) DO UPDATE SET
                path_json = excluded.path_json,
                display_path = excluded.display_path,
                lifetime_generation = excluded.lifetime_generation,
                revision = excluded.revision,
                encoding_json = excluded.encoding_json,
                text_blob = excluded.text_blob,
                digest = excluded.digest,
                updated_at = excluded.updated_at",
            params![
                document.document_id.to_string(),
                document
                    .path
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                document.display_path,
                document.lifetime_generation as i64,
                document.revision.value() as i64,
                serde_json::to_string(&document.encoding)?,
                document.text.as_bytes(),
                digest.as_slice(),
                unix_now() as i64,
            ],
        )?;
        // Deltas at or below this checkpoint are already represented by the
        // full text.
        transaction.execute(
            "DELETE FROM edit_deltas WHERE document_id = ?1 AND revision <= ?2",
            params![
                document.document_id.to_string(),
                document.revision.value() as i64
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn append_delta(
        &mut self,
        document_id: DocumentId,
        base_revision: DocRevision,
        batch: &EditBatch,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO edit_deltas
             (document_id, base_revision, revision, delta_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                document_id.to_string(),
                base_revision.value() as i64,
                batch.expected_revision.next().value() as i64,
                serde_json::to_string(batch)?,
                unix_now() as i64,
            ],
        )?;
        Ok(())
    }

    pub fn load_recovery(
        &self,
        document_id: DocumentId,
    ) -> Result<Option<RecoveredDocument>, StorageError> {
        let row = self
            .conn
            .query_row(
                "SELECT path_json, display_path, lifetime_generation, revision,
                        encoding_json, text_blob, digest, updated_at
                 FROM documents WHERE document_id = ?1",
                params![document_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Vec<u8>>(5)?,
                        row.get::<_, Vec<u8>>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )
            .optional()?;

        let Some((
            path_json,
            display_path,
            generation,
            revision,
            encoding_json,
            text_blob,
            digest,
            updated_at,
        )) = row
        else {
            return Ok(None);
        };

        if digest.as_slice() != blake3::hash(&text_blob).as_bytes() {
            return Err(StorageError::Corrupt(format!(
                "checkpoint for {document_id} failed its digest check"
            )));
        }
        let text = String::from_utf8(text_blob)
            .map_err(|_| StorageError::Corrupt("checkpoint text is not UTF-8".to_string()))?;

        Ok(Some(RecoveredDocument {
            document_id,
            path: path_json
                .map(|json| serde_json::from_str::<NativePath>(&json))
                .transpose()?,
            display_path,
            lifetime_generation: generation.max(0) as u64,
            revision: DocRevision::new(revision.max(0) as u64),
            encoding: serde_json::from_str(&encoding_json)?,
            text,
            digest: digest.try_into().map_err(|_| {
                StorageError::Corrupt("checkpoint digest has the wrong length".to_string())
            })?,
            updated_at: updated_at.max(0) as u64,
        }))
    }

    pub fn list_recoveries(&self) -> Result<Vec<RecoveredDocument>, StorageError> {
        let mut statement = self
            .conn
            .prepare("SELECT document_id FROM documents ORDER BY updated_at DESC")?;
        let ids: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        let mut documents = Vec::new();
        for id in ids {
            let document_id = uuid::Uuid::parse_str(&id)
                .map(DocumentId::from_uuid)
                .map_err(|_| StorageError::Corrupt(format!("invalid document id {id}")))?;
            if let Some(document) = self.load_recovery(document_id)? {
                documents.push(document);
            }
        }
        Ok(documents)
    }

    pub fn remove_recovery(&mut self, document_id: DocumentId) -> Result<(), StorageError> {
        self.conn.execute(
            "DELETE FROM documents WHERE document_id = ?1",
            params![document_id.to_string()],
        )?;
        self.conn.execute(
            "DELETE FROM edit_deltas WHERE document_id = ?1",
            params![document_id.to_string()],
        )?;
        Ok(())
    }

    pub fn save_session(&mut self, documents: &[SessionDocument]) -> Result<(), StorageError> {
        let transaction = self.conn.transaction()?;
        transaction.execute("DELETE FROM session_docs", [])?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO session_docs
                 (position, document_id, path_json, display_path, revision,
                  caret_utf8, scroll_top_line, active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for (position, document) in documents.iter().enumerate() {
                insert.execute(params![
                    position as i64,
                    document.document_id.to_string(),
                    document
                        .path
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    document.display_path,
                    document.revision.value() as i64,
                    document.caret_utf8 as i64,
                    document.scroll_top_line as i64,
                    if document.active { 1 } else { 0 },
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn load_session(&self) -> Result<Vec<SessionDocument>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT document_id, path_json, display_path, revision,
                    caret_utf8, scroll_top_line, active
             FROM session_docs ORDER BY position",
        )?;
        let mut rows = statement.query([])?;
        let mut documents = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let document_id = uuid::Uuid::parse_str(&id)
                .map(DocumentId::from_uuid)
                .map_err(|_| StorageError::Corrupt(format!("invalid session document id {id}")))?;
            let path_json: Option<String> = row.get(1)?;
            documents.push(SessionDocument {
                document_id,
                path: path_json
                    .map(|json| serde_json::from_str::<NativePath>(&json))
                    .transpose()?,
                display_path: row.get(2)?,
                revision: DocRevision::new(row.get::<_, i64>(3)?.max(0) as u64),
                caret_utf8: row.get::<_, i64>(4)?.max(0) as u64,
                scroll_top_line: row.get::<_, i64>(5)?.max(0) as u64,
                active: row.get::<_, i64>(6)? != 0,
            });
        }
        Ok(documents)
    }

    pub fn touch_recent(
        &mut self,
        path: &NativePath,
        display_path: &str,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO recent_files(path_json, display_path, opened_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(path_json) DO UPDATE SET
                display_path = excluded.display_path,
                opened_at = excluded.opened_at",
            params![
                serde_json::to_string(path)?,
                display_path,
                unix_now() as i64
            ],
        )?;
        Ok(())
    }

    pub fn list_recent(&self, limit: usize) -> Result<Vec<RecentFile>, StorageError> {
        let mut statement = self
            .conn
            .prepare("SELECT path_json, display_path, opened_at FROM recent_files ORDER BY opened_at DESC LIMIT ?1")?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut files = Vec::new();
        for row in rows {
            let (path_json, display_path, opened_at) = row?;
            files.push(RecentFile {
                path: serde_json::from_str(&path_json)?,
                display_path,
                opened_at: opened_at.max(0) as u64,
            });
        }
        Ok(files)
    }

    pub fn remove_recent(&mut self, path: &NativePath) -> Result<(), StorageError> {
        self.conn.execute(
            "DELETE FROM recent_files WHERE path_json = ?1",
            params![serde_json::to_string(path)?],
        )?;
        Ok(())
    }

    pub fn prepare_save_op(
        &mut self,
        document_id: DocumentId,
        revision: DocRevision,
        target: &NativePath,
        temp: &NativePath,
        rollback: Option<&NativePath>,
        output_digest: Option<[u8; 32]>,
    ) -> Result<i64, StorageError> {
        self.conn.execute(
            "INSERT INTO save_ops
             (document_id, revision, target_json, temp_json, rollback_json,
              output_digest, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'prepared', ?7)",
            params![
                document_id.to_string(),
                revision.value() as i64,
                serde_json::to_string(target)?,
                serde_json::to_string(temp)?,
                rollback.map(serde_json::to_string).transpose()?,
                output_digest.as_ref().map(|digest| digest.as_slice()),
                unix_now() as i64,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_save_op(&mut self, id: i64, state: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE save_ops SET state = ?1 WHERE id = ?2",
            params![state, id],
        )?;
        Ok(())
    }

    pub fn pending_save_ops(&self) -> Result<Vec<SaveJournalEntry>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT id, document_id, revision, target_json, temp_json,
                    rollback_json, output_digest, state, created_at
             FROM save_ops WHERE state IN ('prepared', 'needs_reconciliation')
             ORDER BY id",
        )?;
        let mut rows = statement.query([])?;
        let mut ops = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(1)?;
            let document_id = uuid::Uuid::parse_str(&id)
                .map(DocumentId::from_uuid)
                .map_err(|_| StorageError::Corrupt(format!("invalid save op document id {id}")))?;
            let rollback_json: Option<String> = row.get(5)?;
            let digest: Option<Vec<u8>> = row.get(6)?;
            ops.push(SaveJournalEntry {
                id: row.get(0)?,
                document_id,
                revision: DocRevision::new(row.get::<_, i64>(2)?.max(0) as u64),
                target: serde_json::from_str(&row.get::<_, String>(3)?)?,
                temp: serde_json::from_str(&row.get::<_, String>(4)?)?,
                rollback: rollback_json
                    .map(|json| serde_json::from_str::<NativePath>(&json))
                    .transpose()?,
                output_digest: digest
                    .map(|bytes| {
                        bytes.try_into().map_err(|_| {
                            StorageError::Corrupt("save op digest has the wrong length".to_string())
                        })
                    })
                    .transpose()?,
                state: row.get(7)?,
                created_at: row.get::<_, i64>(8)?.max(0) as u64,
            });
        }
        Ok(ops)
    }

    /// Deletes checkpoints and deltas older than `days`. A zero value disables
    /// age-based cleanup; callers must still enforce the disk budget.
    pub fn purge_older_than(&mut self, days: u64) -> Result<usize, StorageError> {
        if days == 0 {
            return Ok(0);
        }
        let cutoff = unix_now().saturating_sub(days.saturating_mul(24 * 60 * 60));
        let removed_docs = self.conn.execute(
            "DELETE FROM documents WHERE updated_at < ?1",
            params![cutoff as i64],
        )?;
        let removed_deltas = self.conn.execute(
            "DELETE FROM edit_deltas WHERE created_at < ?1",
            params![cutoff as i64],
        )?;
        Ok(removed_docs + removed_deltas)
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notepust_contracts::Utf8Range;
    use notepust_contracts::{BomPolicy, EncodingId, SnapshotId, TextEdit, TransactionId};

    fn sample_document(text: &str) -> RecoveredDocument {
        RecoveredDocument {
            document_id: DocumentId::new(),
            path: Some(NativePath::from(std::path::Path::new("C:\\tmp\\a.txt"))),
            display_path: Some("C:\\tmp\\a.txt".to_string()),
            lifetime_generation: 1,
            revision: DocRevision::new(4),
            encoding: EncodingSpec::new(EncodingId::Utf8, BomPolicy::None),
            text: text.to_string(),
            digest: *blake3::hash(text.as_bytes()).as_bytes(),
            updated_at: unix_now(),
        }
    }

    #[test]
    fn checkpoint_roundtrips_and_detects_corruption() {
        let mut db = RecoveryDb::open_in_memory().unwrap();
        let document = sample_document("第一行\r\nsecond");
        db.checkpoint(&document).unwrap();

        let loaded = db.load_recovery(document.document_id).unwrap().unwrap();
        assert_eq!(loaded.text, document.text);
        assert_eq!(loaded.revision, document.revision);
        assert_eq!(loaded.encoding, document.encoding);

        let all = db.list_recoveries().unwrap();
        assert_eq!(all.len(), 1);

        db.remove_recovery(document.document_id).unwrap();
        assert!(db.load_recovery(document.document_id).unwrap().is_none());
    }

    #[test]
    fn source_chunks_roundtrip_and_validate() {
        let mut db = RecoveryDb::open_in_memory().unwrap();
        let id = SourceSnapshotId::new();
        let chunks = vec![b"abc".to_vec(), b"def".to_vec()];
        let digest = db.store_source_snapshot(id, &chunks).unwrap();
        assert_eq!(digest, *blake3::hash(b"abcdef").as_bytes());
        assert_eq!(db.load_source_snapshot(id).unwrap(), chunks);
    }

    #[test]
    fn session_and_recent_files_roundtrip() {
        let mut db = RecoveryDb::open_in_memory().unwrap();
        let doc = SessionDocument {
            document_id: DocumentId::new(),
            path: None,
            display_path: "未命名 1".to_string(),
            revision: DocRevision::new(1),
            caret_utf8: 5,
            scroll_top_line: 2,
            active: true,
        };
        db.save_session(std::slice::from_ref(&doc)).unwrap();
        let loaded = db.load_session().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].display_path, "未命名 1");
        assert!(loaded[0].active);

        let path = NativePath::from(std::path::Path::new("C:\\tmp\\b.txt"));
        db.touch_recent(&path, "b.txt").unwrap();
        let recent = db.list_recent(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].path, path);
    }

    #[test]
    fn checkpoint_deletes_covered_deltas() {
        let mut db = RecoveryDb::open_in_memory().unwrap();
        let document = sample_document("abc");
        db.checkpoint(&document).unwrap();

        let mut batch = EditBatch::new(document.document_id, DocRevision::new(4), "editor.insert");
        batch.transaction_id = TransactionId::new();
        batch.push(TextEdit::new(Utf8Range::new(0u64, 1u64).unwrap(), "x"));
        db.append_delta(document.document_id, DocRevision::new(4), &batch)
            .unwrap();

        let mut later = document.clone();
        later.revision = DocRevision::new(5);
        later.text = "xbc".to_string();
        db.checkpoint(&later).unwrap();

        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM edit_deltas", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn save_journal_tracks_pending_operations() {
        let mut db = RecoveryDb::open_in_memory().unwrap();
        let target = NativePath::from(std::path::Path::new("C:\\tmp\\a.txt"));
        let temp = NativePath::from(std::path::Path::new("C:\\tmp\\a.tmp"));
        let id = db
            .prepare_save_op(
                DocumentId::new(),
                DocRevision::new(1),
                &target,
                &temp,
                None,
                Some([7; 32]),
            )
            .unwrap();
        assert_eq!(db.pending_save_ops().unwrap().len(), 1);
        db.update_save_op(id, "succeeded").unwrap();
        assert!(db.pending_save_ops().unwrap().is_empty());
    }

    #[test]
    fn snapshot_id_is_usable_as_an_identifier() {
        let id = SnapshotId::new();
        assert_eq!(id.as_uuid().get_version_num(), 4);
    }
}
