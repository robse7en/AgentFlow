//! Text files read and written the way Python's `Path.read_text` / `Path.write_text` do with
//! `encoding="utf-8"` and no `newline=` argument, so files the port writes are byte-identical to
//! the Python backend's and text read back (requirement documents, digests) matches too:
//!
//! - reading translates `\r\n` and lone `\r` to `\n` (universal newlines);
//! - writing translates `\n` to the platform line ending (`\r\n` on Windows).

use std::io;
use std::path::Path;

pub fn read_text(path: &Path) -> io::Result<String> {
    let text = std::fs::read_to_string(path)?;
    Ok(if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text })
}

pub fn write_text(path: &Path, text: &str) -> io::Result<()> {
    std::fs::write(path, platform_newlines(text))
}

/// `\n` -> `\r\n` on Windows; unchanged elsewhere.
pub fn platform_newlines(text: &str) -> String {
    if cfg!(windows) { text.replace('\n', "\r\n") } else { text.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_like_python() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        write_text(&path, "one\ntwo\n").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, if cfg!(windows) { b"one\r\ntwo\r\n".to_vec() } else { b"one\ntwo\n".to_vec() });
        std::fs::write(&path, "a\r\nb\rc\n").unwrap();
        assert_eq!(read_text(&path).unwrap(), "a\nb\nc\n");
    }
}
