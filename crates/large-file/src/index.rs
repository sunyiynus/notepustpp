use notepust_contracts::EncodingSpec;

use crate::read_at::FileReader;
use crate::scan::scan_raw_lines;

pub const LINE_CHECKPOINT_INTERVAL: u64 = 4_096;

/// Sparse anchor: a line number and the raw byte offset where it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexCheckpoint {
    pub line: u64,
    pub raw_start: u64,
}

/// Incremental sparse index over the raw file.
///
/// It never stores every line. Jumping to a line decodes from the closest
/// checkpoint, bounded by `LINE_CHECKPOINT_INTERVAL` lines.
#[derive(Debug)]
pub struct LineIndex {
    spec: EncodingSpec,
    generation: u64,
    file_len: u64,
    scanned_raw: u64,
    pending: Vec<u8>,
    lines_found: u64,
    complete: bool,
    checkpoints: Vec<IndexCheckpoint>,
}

impl LineIndex {
    pub fn new(spec: EncodingSpec, generation: u64, file_len: u64) -> Self {
        let start = spec.bom.len() as u64;
        Self {
            spec,
            generation,
            file_len,
            scanned_raw: start,
            pending: Vec::new(),
            lines_found: 0,
            complete: false,
            checkpoints: vec![IndexCheckpoint {
                line: 0,
                raw_start: start,
            }],
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn spec(&self) -> &EncodingSpec {
        &self.spec
    }

    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    /// Raw offset where the next complete line starts.
    pub fn scanned_raw(&self) -> u64 {
        self.scanned_raw
    }

    /// Offset of the next byte that has not been read.
    pub fn read_position(&self) -> u64 {
        self.scanned_raw + self.pending.len() as u64
    }

    pub fn lines_found(&self) -> u64 {
        self.lines_found
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn total_lines(&self) -> Option<u64> {
        self.complete.then_some(self.lines_found)
    }

    pub fn checkpoints(&self) -> &[IndexCheckpoint] {
        &self.checkpoints
    }

    pub fn checkpoint_for_line(&self, line: u64) -> IndexCheckpoint {
        match self
            .checkpoints
            .binary_search_by_key(&line, |checkpoint| checkpoint.line)
        {
            Ok(index) => self.checkpoints[index],
            Err(0) => self.checkpoints[0],
            Err(index) => self.checkpoints[index - 1],
        }
    }

    /// Reads the next chunk from the file and updates the index.
    ///
    /// Returns `(bytes_fed, new_lines, reached_eof)`.
    pub fn scan_next(
        &mut self,
        reader: &FileReader,
        max_bytes: usize,
    ) -> std::io::Result<(usize, u64, bool)> {
        if self.complete {
            return Ok((0, 0, true));
        }
        let read_at = self.read_position();
        let remaining = self.file_len.saturating_sub(read_at) as usize;
        let to_read = max_bytes.min(remaining);
        if to_read == 0 {
            self.finish();
            return Ok((0, 0, true));
        }

        let mut new_bytes = vec![0u8; to_read];
        let read = reader.read_exact_at(read_at, &mut new_bytes)?;
        new_bytes.truncate(read);
        let last = read_at + read as u64 >= self.file_len;
        let before = self.lines_found;
        self.feed(&new_bytes, last);
        Ok((read, self.lines_found - before, last))
    }

    /// Feeds already-read bytes that begin at [`LineIndex::read_position`].
    pub fn feed(&mut self, bytes: &[u8], last: bool) {
        if self.complete || bytes.is_empty() && !last {
            return;
        }
        let mut data = Vec::with_capacity(self.pending.len() + bytes.len());
        data.extend_from_slice(&self.pending);
        data.extend_from_slice(bytes);

        let scan = scan_raw_lines(
            &data,
            self.scanned_raw,
            self.spec.encoding,
            self.lines_found,
            last,
        );

        for line in &scan.lines {
            if line.number % LINE_CHECKPOINT_INTERVAL == 0
                && self.checkpoints.last().map(|checkpoint| checkpoint.line) != Some(line.number)
            {
                self.checkpoints.push(IndexCheckpoint {
                    line: line.number,
                    raw_start: line.start,
                });
            }
        }

        if let Some(last_line) = scan.lines.last() {
            self.lines_found = last_line.number + 1;
        }

        self.scanned_raw += scan.consumed;
        self.pending.clear();
        if last {
            self.complete = true;
            self.scanned_raw = self.file_len;
        } else {
            self.pending
                .extend_from_slice(&data[scan.consumed as usize..]);
        }
    }

    /// Handles an empty file and marks the index complete.
    pub fn finish(&mut self) {
        if !self.complete {
            if self.lines_found == 0 {
                // One empty logical line, consistent with the editor.
                self.lines_found = 1;
                self.checkpoints = vec![IndexCheckpoint {
                    line: 0,
                    raw_start: self.spec.bom.len() as u64,
                }];
            }
            self.complete = true;
            self.pending.clear();
            self.scanned_raw = self.file_len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notepust_contracts::{BomPolicy, EncodingId};
    use std::io::Write;

    #[test]
    fn index_counts_lines_and_checkpoints() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"a\nb\nc\n").unwrap();
        file.flush().unwrap();
        let reader = FileReader::open(file.path()).unwrap();
        let len = std::fs::metadata(file.path()).unwrap().len();
        let mut index = LineIndex::new(EncodingSpec::utf8(), 0, len);
        index.scan_next(&reader, 1024).unwrap();
        assert!(index.is_complete());
        assert_eq!(index.total_lines(), Some(4));
        assert_eq!(index.scanned_raw(), len);
    }

    #[test]
    fn crlf_split_across_chunks_is_counted_once() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"aa\r\nbb\r\ncc").unwrap();
        file.flush().unwrap();
        let reader = FileReader::open(file.path()).unwrap();
        let len = std::fs::metadata(file.path()).unwrap().len();
        let mut index = LineIndex::new(EncodingSpec::utf8(), 0, len);
        index.scan_next(&reader, 3).unwrap();
        index.scan_next(&reader, 3).unwrap();
        index.scan_next(&reader, 3).unwrap();
        index.scan_next(&reader, 10).unwrap();
        assert!(index.is_complete());
        assert_eq!(index.total_lines(), Some(3));
    }

    #[test]
    fn checkpoint_lookup_is_sparse() {
        let spec = EncodingSpec::new(EncodingId::Utf8, BomPolicy::None);
        let mut index = LineIndex::new(spec, 0, 0);
        index.feed(b"a\nb\nc\nd\n", true);
        assert_eq!(index.checkpoint_for_line(0).line, 0);
        assert_eq!(index.checkpoint_for_line(100).line, 0);
        assert_eq!(index.total_lines(), Some(5));
    }
}
