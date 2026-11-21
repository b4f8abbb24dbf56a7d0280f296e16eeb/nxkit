//! Positional file I/O (`pread`/`pwrite`): `std::os::unix::fs::FileExt` on Unix, the same two calls on Windows over
//! `seek_read`/`seek_write`, which take the offset with the call and so stay safe between threads.

#[cfg(unix)]
pub use std::os::unix::fs::FileExt;

/// `read_exact_at` and `write_all_at` as on Unix.
#[cfg(windows)]
pub trait FileExt {
    /// Reads `buf.len()` bytes at `offset`, or fails with `UnexpectedEof`.
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()>;
    /// Writes all of `buf` at `offset`.
    fn write_all_at(&self, buf: &[u8], offset: u64) -> std::io::Result<()>;
}

#[cfg(windows)]
impl FileExt for std::fs::File {
    fn read_exact_at(&self, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
        use std::os::windows::fs::FileExt as _;
        while !buf.is_empty() {
            match self.seek_read(buf, offset) {
                Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn write_all_at(&self, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
        use std::os::windows::fs::FileExt as _;
        while !buf.is_empty() {
            match self.seek_write(buf, offset) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    buf = &buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::FileExt;

    #[test]
    fn write_and_read_at_offsets() {
        let path = std::env::temp_dir().join(format!("nxb-fileext-{}", std::process::id()));
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        f.write_all_at(b"world", 6).unwrap();
        f.write_all_at(b"hello ", 0).unwrap();
        let mut buf = [0u8; 5];
        f.read_exact_at(&mut buf, 6).unwrap();
        assert_eq!(&buf, b"world");
        let err = f.read_exact_at(&mut buf, 8).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
        drop(f);
        std::fs::remove_file(&path).unwrap();
    }
}
