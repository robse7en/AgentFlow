//! Path comparison the way the Python backend does it with `Path.resolve()` and
//! `os.path.normcase`, in one place, so every module compares paths identically.
//!
//! - [`resolve`]: absolute, with the deepest existing ancestor canonicalized (symlinks, junctions
//!   and 8.3 short names expanded, on-disk case) and the missing remainder appended, like Python's
//!   non-strict `resolve()`. The `\\?\` prefix Windows canonicalization adds is removed.
//! - [`same_path`]: two paths resolve to the same location (case-insensitive on Windows).
//! - [`is_within`]: a path is a directory or inside it, by name; used to decide which processes
//!   to stop, so it never matches a sibling that merely shares a prefix.
//! - [`linked`]: why a path must not be trusted as a plain directory (a `.`/`..` component, or a
//!   symlink/junction among its existing ancestors).

use std::path::{Component, Path, PathBuf};

/// `path` made absolute, with its existing part canonicalized and the rest appended unchanged.
pub fn resolve(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut existing = absolute.as_path();
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(existing) {
            let mut resolved = strip_verbatim(canonical);
            resolved.extend(rest.iter().rev());
            return normalize(&resolved);
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return normalize(&absolute),
        }
    }
}

/// Whether two paths resolve to the same location.
pub fn same_path(left: impl AsRef<Path>, right: impl AsRef<Path>) -> bool {
    key(&resolve(left.as_ref())) == key(&resolve(right.as_ref()))
}

/// Whether `path` is `directory` or inside it, comparing normalized names (case-insensitively on
/// Windows) without touching the filesystem. `C:\w\DE-10` is not within `C:\w\DE-1`.
pub fn is_within(path: &Path, directory: &Path) -> bool {
    let (path, directory) = (key(&normalize(path)), key(&normalize(directory)));
    let separator = std::path::MAIN_SEPARATOR;
    path == directory || path.starts_with(&format!("{}{separator}", directory.trim_end_matches(separator)))
}

/// Why `path` must not be treated as a plain directory, or None when it can be. Replaces the
/// Python backend's `path.resolve() != path.absolute()`, which also rejected 8.3 short names
/// (`C:\Users\ROBROD~1\...`) and paths typed in a different case than on disk.
pub fn linked(path: &Path) -> Option<String> {
    if path.components().any(|c| matches!(c, Component::CurDir | Component::ParentDir)) {
        return Some(format!("{} contains . or .. components", path.display()));
    }
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    for ancestor in absolute.ancestors() {
        if let Ok(metadata) = std::fs::symlink_metadata(ancestor)
            && is_link(&metadata)
        {
            return Some(format!("{} is a symbolic link or junction", ancestor.display()));
        }
    }
    None
}

/// Symbolic links and, on Windows, junctions: std reports every name-surrogate reparse point as a
/// symlink (checked by `linked_rejects_a_path_through_a_junction`). Other reparse points, such as
/// OneDrive placeholders and deduplicated files, are ordinary directories and files here, as they
/// were to the Python backend.
fn is_link(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// Lexical normalization: separators unified, `.` dropped and `..` applied (`os.path.normpath`).
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The comparison form of a normalized path: case-folded on Windows, without a trailing separator.
fn key(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = if cfg!(windows) { text.replace('/', "\\").to_lowercase() } else { text.into_owned() };
    let separator = std::path::MAIN_SEPARATOR;
    if text.len() > 1 && text.ends_with(separator) && !text.ends_with(&format!(":{separator}")) {
        text.trim_end_matches(separator).to_string()
    } else {
        text
    }
}

/// `\\?\C:\x` -> `C:\x`, `\\?\UNC\server\share` -> `\\server\share`.
pub(crate) fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        PathBuf::from(rest)
    } else {
        path
    }
}

/// Python's `str(Path(path))`: the platform separator throughout, with empty and `.` segments and a
/// trailing separator dropped (`..` is kept). The API reports configured paths in this form.
pub fn py_str(path: &Path) -> String {
    let separator = std::path::MAIN_SEPARATOR;
    let mut prefix = String::new();
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => prefix = p.as_os_str().to_string_lossy().replace('/', &separator.to_string()),
            Component::RootDir => prefix.push(separator),
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".into()),
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
        }
    }
    let joined = format!("{prefix}{}", parts.join(&separator.to_string()));
    if joined.is_empty() { ".".into() } else { joined }
}

/// Whether `path` names a filesystem root (`C:\`, `/`, `\\server\share\`).
pub fn is_root(path: &Path) -> bool {
    normalize(path).parent().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn py_str_matches_python_windows_paths() {
        // Each pair is (input, str(Path(input))) from CPython 3.13 on Windows.
        let cases = [
            ("C:/x/home", r"C:\x\home"),
            ("C:/x//home/./db.sqlite", r"C:\x\home\db.sqlite"),
            (r"C:\x\home\", r"C:\x\home"),
            ("C:/", r"C:\"),
            ("relative/../x", r"relative\..\x"),
            ("", "."),
            (r"\\server\share\dir", r"\\server\share\dir"),
        ];
        for (input, expected) in cases {
            assert_eq!(py_str(Path::new(input)), expected, "{input}");
        }
    }

    #[test]
    fn within_does_not_match_a_sibling_that_shares_a_prefix() {
        let base = std::env::temp_dir().join("w");
        assert!(is_within(&base.join("DE-1"), &base.join("DE-1")));
        assert!(is_within(&base.join("DE-1").join("nested"), &base.join("DE-1")));
        assert!(!is_within(&base.join("DE-10"), &base.join("DE-1")));
        assert!(!is_within(&base.join("DE-1x").join("a"), &base.join("DE-1")));
        assert!(!is_within(&base, &base.join("DE-1")));
    }

    #[test]
    fn within_ignores_a_trailing_separator_and_dot_segments() {
        let base = std::env::temp_dir().join("w");
        let with_slash = PathBuf::from(format!("{}{}", base.join("DE-1").display(), std::path::MAIN_SEPARATOR));
        assert!(is_within(&base.join("DE-1").join("a"), &with_slash));
        assert!(is_within(&base.join("DE-1").join("x").join("..").join("a"), &base.join("DE-1")));
        assert!(!is_within(&base.join("DE-1").join("..").join("DE-2"), &base.join("DE-1")));
    }

    #[cfg(windows)]
    #[test]
    fn within_is_case_insensitive_and_slash_agnostic_on_windows() {
        assert!(is_within(Path::new(r"c:\WORK\de-1\Nested"), Path::new(r"C:\work\DE-1")));
        assert!(is_within(Path::new("C:/work/DE-1/a"), Path::new(r"C:\work\DE-1")));
        assert!(!is_within(Path::new(r"D:\work\DE-1"), Path::new(r"C:\work\DE-1")));
    }

    #[test]
    fn a_drive_root_contains_everything_on_it_so_callers_must_refuse_it() {
        let root = std::env::temp_dir().ancestors().last().unwrap().to_path_buf();
        assert!(is_root(&root));
        assert!(is_within(&std::env::temp_dir(), &root));
        assert!(!is_root(&std::env::temp_dir()));
    }

    #[test]
    fn same_path_matches_differently_spelled_paths() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::create_dir(&a).unwrap();
        assert!(same_path(&a, a.join("x").join("..")));
        assert!(same_path(a.join("missing").join("tail"), a.join("missing").join("tail")));
        assert!(!same_path(&a, dir.path().join("b")));
        #[cfg(windows)]
        {
            let forward = a.to_string_lossy().replace('\\', "/").to_uppercase();
            assert!(same_path(&a, Path::new(&forward)));
        }
    }

    #[test]
    fn resolve_keeps_a_missing_tail_and_drops_the_verbatim_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve(&dir.path().join("not").join("there"));
        assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
        assert!(resolved.ends_with(Path::new("not").join("there")));
    }

    #[test]
    fn linked_rejects_dot_components_and_accepts_a_plain_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(linked(&dir.path().join("a").join("..").join("b")).is_some());
        // A `.` inside a path is dropped by Path itself (as by Python's pathlib): harmless.
        assert_eq!(linked(&dir.path().join(".").join("b")), None);
        assert_eq!(linked(&dir.path().join("plain")), None);
    }

    #[cfg(windows)]
    #[test]
    fn linked_rejects_a_path_through_a_junction() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let junction = dir.path().join("junction");
        std::fs::create_dir(&target).unwrap();
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .output()
            .unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        // std's own view, not ours: does it report a junction as a symlink?
        let metadata = std::fs::symlink_metadata(&junction).unwrap();
        assert!(metadata.file_type().is_symlink(), "std does not report a junction as a symlink");
        assert!(is_link(&metadata), "a junction must count as a link");
        assert!(linked(&junction.join("inside")).is_some());
        assert!(linked(&junction).is_some());
    }

    /// The Python guard rejected 8.3 short names; this one accepts them as the same directory.
    #[cfg(windows)]
    #[test]
    fn short_8_3_names_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let long = dir.path().join("a long directory name");
        std::fs::create_dir(&long).unwrap();
        let output = std::process::Command::new("cmd")
            .args(["/c", "for", "%I", "in"])
            .arg(format!("(\"{}\")", long.display()))
            .args(["do", "@echo", "%~sI"])
            .output()
            .unwrap();
        let short = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if short.is_empty() || short.eq_ignore_ascii_case(&long.to_string_lossy()) {
            eprintln!("no 8.3 name on this volume; skipped");
            return;
        }
        assert_eq!(linked(Path::new(&short)), None);
        assert!(same_path(Path::new(&short), &long));
    }
}
