use std::fs::File;
use std::io;
use std::path::Path;

/// Positional file reader. Implementations must not mix the sequential file
/// cursor with offset reads on Windows.
pub struct FileReader {
    file: File,
}

impl FileReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
        })
    }

    /// Reads at `offset` into `buffer`, retrying short reads and interrupts.
    pub fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let mut filled = 0usize;
        while filled < buffer.len() {
            match self.read_at(offset + filled as u64, &mut buffer[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(filled)
    }

    #[cfg(windows)]
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        use std::os::windows::fs::FileExt;
        self.file.seek_read(buffer, offset)
    }

    #[cfg(unix)]
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        use std::os::unix::fs::FileExt;
        self.file.read_at(buffer, offset)
    }

    #[cfg(not(any(windows, unix)))]
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = &self.file;
        file.seek(SeekFrom::Start(offset))?;
        file.read(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn positional_reads_handle_short_reads_and_boundaries() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"0123456789").unwrap();
        file.flush().unwrap();
        let reader = FileReader::open(file.path()).unwrap();
        let mut buffer = [0u8; 4];
        assert_eq!(reader.read_exact_at(3, &mut buffer).unwrap(), 4);
        assert_eq!(&buffer, b"3456");
        let mut tail = [0u8; 8];
        assert_eq!(reader.read_exact_at(8, &mut tail).unwrap(), 2);
        assert_eq!(&tail[..2], b"89");
    }
}
