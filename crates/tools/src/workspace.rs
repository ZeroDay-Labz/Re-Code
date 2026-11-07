//! A workspace root that confines every file operation beneath it.
//!
//! Opening a file uses the kernel's `openat2` with `RESOLVE_BENEATH`, so the lookup itself refuses
//! to cross `..` or a symlink that points outside the workspace — there's no gap between the check
//! and the open. Writes and deletes add a lexical-plus-canonical check for the same guarantee.

use rustix::fs::{Mode, OFlags, ResolveFlags};
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

/// Where a path really is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Location {
    /// Inside the project, relative to its root (`.` for the root itself).
    Inside(String),
    /// Elsewhere on this computer, as an absolute path with symlinks resolved.
    Outside(PathBuf),
}

impl Location {
    #[must_use]
    pub fn is_outside(&self) -> bool {
        matches!(self, Self::Outside(_))
    }

    /// How it's shown: relative inside the project, `~/…` or absolute outside.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Inside(relative) => relative.clone(),
            Self::Outside(path) => tilde(path),
        }
    }

    /// One stable key per file, for checkpoints.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Inside(relative) => relative.clone(),
            Self::Outside(path) => path.to_string_lossy().into_owned(),
        }
    }
}

/// `~/notes/a.md` for a path under the home folder.
#[must_use]
pub fn tilde(path: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from).and_then(|home| path.strip_prefix(home).ok().map(Path::to_owned))
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn expand_home(path: &str) -> Result<PathBuf, String> {
    if path == "~" || path.starts_with("~/") {
        let home = std::env::var_os("HOME").ok_or("the home folder isn't known (HOME isn't set)")?;
        return Ok(PathBuf::from(home).join(path.trim_start_matches('~').trim_start_matches('/')));
    }
    Ok(PathBuf::from(path))
}

/// Removes `.` and `..` without touching the disk.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Resolves symlinks in the part of the path that exists, keeping the rest as written.
fn resolve_existing(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut out = real;
            for part in missing.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match existing.file_name() {
            Some(name) => missing.push(name.to_owned()),
            None => return path.to_path_buf(),
        }
        if !existing.pop() {
            return path.to_path_buf();
        }
    }
}

fn io_error(location: &Location, error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => format!("{} doesn't exist", location.display()),
        std::io::ErrorKind::PermissionDenied => {
            format!("{} can't be accessed by your user account", location.display())
        }
        _ => format!("can't use {}: {error}", location.display()),
    }
}

/// A confined workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Creates a workspace at `root`, which must exist. The path is canonicalised.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref();
        let root = root.canonicalize().map_err(|error| format!("can't open workspace {}: {error}", root.display()))?;
        if !root.is_dir() {
            return Err(format!("{} isn't a directory", root.display()));
        }
        Ok(Self { root })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Lexically resolves a relative path and rejects anything that would escape the root.
    fn relative(&self, path: &str) -> Result<PathBuf, String> {
        let path = Path::new(path);
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !normalized.pop() {
                        return Err("that path points outside the workspace".to_owned());
                    }
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err("use a path relative to the workspace, not an absolute path".to_owned());
                }
            }
        }
        Ok(normalized)
    }

    /// Opens a path beneath the root, with the kernel enforcing confinement.
    fn open_beneath(&self, path: &str, flags: OFlags) -> Result<OwnedFd, String> {
        let relative = self.relative(path)?;
        let relative = if relative.as_os_str().is_empty() { PathBuf::from(".") } else { relative };
        let dir = rustix::fs::open(&self.root, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())
            .map_err(|error| format!("can't open the workspace: {error}"))?;
        rustix::fs::openat2(&dir, &relative, flags | OFlags::CLOEXEC, Mode::empty(), ResolveFlags::BENEATH).map_err(
            |error| match error {
                rustix::io::Errno::XDEV | rustix::io::Errno::LOOP => {
                    "that path points outside the workspace".to_owned()
                }
                rustix::io::Errno::NOENT => format!("{path} doesn't exist"),
                rustix::io::Errno::ISDIR => format!("{path} is a directory"),
                other => format!("can't open {path}: {other}"),
            },
        )
    }

    /// The absolute path for a write/delete, after confinement. Also canonicalises the existing
    /// parent so a symlinked directory can't redirect outside the root.
    fn writable_path(&self, path: &str) -> Result<PathBuf, String> {
        let relative = self.relative(path)?;
        if relative.as_os_str().is_empty() {
            return Err("that path is the workspace root".to_owned());
        }
        let full = self.root.join(&relative);
        if let Some(parent) = full.parent()
            && parent.exists()
        {
            let canonical =
                parent.canonicalize().map_err(|error| format!("can't resolve {}: {error}", parent.display()))?;
            if !canonical.starts_with(&self.root) {
                return Err("that path points outside the workspace".to_owned());
            }
        }
        Ok(full)
    }

    /// Reads a file.
    pub fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        use std::io::Read;
        let fd = self.open_beneath(path, OFlags::RDONLY)?;
        let mut file = File::from(fd);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|error| format!("can't read {path}: {error}"))?;
        Ok(bytes)
    }

    /// Works out where a path really is, the way the operating system will: `~/` and absolute paths,
    /// `..`, and symlinks are all resolved, so the location that's approved is the one that's touched.
    /// Inside the project it stays a relative path, and is opened with the kernel's confinement as
    /// before; anywhere else it's an absolute path, which the engine only uses with the user's approval.
    pub fn locate(&self, path: &str) -> Result<Location, String> {
        let path = path.trim();
        if path.is_empty() {
            return Err("give a path".to_owned());
        }
        let expanded = expand_home(path)?;
        let candidate = if expanded.is_absolute() { expanded } else { self.root.join(expanded) };
        let resolved = resolve_existing(&lexical(&candidate));
        match resolved.strip_prefix(&self.root) {
            Ok(relative) => {
                let relative = relative.to_string_lossy().into_owned();
                Ok(Location::Inside(if relative.is_empty() { ".".to_owned() } else { relative }))
            }
            Err(_) => Ok(Location::Outside(resolved)),
        }
    }

    /// Reads a file wherever it is.
    pub fn read_at(&self, location: &Location) -> Result<Vec<u8>, String> {
        match location {
            Location::Inside(relative) => self.read(relative),
            Location::Outside(path) => {
                if path.is_dir() {
                    return Err(format!("{} is a directory", location.display()));
                }
                std::fs::read(path).map_err(|error| io_error(location, &error))
            }
        }
    }

    /// Lists a directory wherever it is.
    pub fn list_at(&self, location: &Location) -> Result<Vec<String>, String> {
        match location {
            Location::Inside(relative) => self.list(relative),
            Location::Outside(path) => {
                let entries = std::fs::read_dir(path).map_err(|error| io_error(location, &error))?;
                Ok(entries
                    .flatten()
                    .map(|entry| {
                        let suffix = if entry.file_type().is_ok_and(|kind| kind.is_dir()) { "/" } else { "" };
                        format!("{}{suffix}", entry.file_name().to_string_lossy())
                    })
                    .collect())
            }
        }
    }

    /// Reads a file wherever it is, or `None` if nothing is there.
    pub fn read_existing_at(&self, location: &Location) -> Result<Option<Vec<u8>>, String> {
        match location {
            Location::Inside(relative) => self.read_existing(relative),
            Location::Outside(path) => match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(io_error(location, &error)),
                Ok(_) => self.read_at(location).map(Some),
            },
        }
    }

    /// Writes a file wherever it is, creating parent directories.
    pub fn write_at(&self, location: &Location, content: &[u8]) -> Result<(), String> {
        match location {
            Location::Inside(relative) => self.write(relative, content),
            Location::Outside(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| io_error(location, &error))?;
                }
                std::fs::write(path, content).map_err(|error| io_error(location, &error))
            }
        }
    }

    /// Deletes a file wherever it is (never a directory).
    pub fn delete_at(&self, location: &Location) -> Result<(), String> {
        match location {
            Location::Inside(relative) => self.delete(relative),
            Location::Outside(path) => {
                if path.is_dir() {
                    return Err(format!("{} is a directory; only files can be deleted", location.display()));
                }
                std::fs::remove_file(path).map_err(|error| io_error(location, &error))
            }
        }
    }

    /// The normalised relative form of a path (`./src/../a.rs` → `a.rs`), so one file has one key.
    pub fn normalize(&self, path: &str) -> Result<String, String> {
        let relative = self.relative(path)?;
        if relative.as_os_str().is_empty() {
            return Err("that path is the workspace root".to_owned());
        }
        Ok(relative.to_string_lossy().into_owned())
    }

    /// Reads a file, or `None` if nothing is there.
    pub fn read_existing(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        let full = self.writable_path(path)?;
        match std::fs::symlink_metadata(&full) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("can't check {path}: {error}")),
            Ok(_) => self.read(path).map(Some),
        }
    }

    /// Lists a directory's entries (names, with a trailing `/` for subdirectories).
    pub fn list(&self, path: &str) -> Result<Vec<String>, String> {
        let fd = self.open_beneath(path, OFlags::RDONLY | OFlags::DIRECTORY)?;
        let dir = rustix::fs::Dir::read_from(&fd).map_err(|error| format!("can't list {path}: {error}"))?;
        let mut out = Vec::new();
        for entry in dir {
            let entry = entry.map_err(|error| format!("can't list {path}: {error}"))?;
            let name = entry.file_name().to_string_lossy();
            if name == "." || name == ".." {
                continue;
            }
            let suffix = if entry.file_type() == rustix::fs::FileType::Directory { "/" } else { "" };
            out.push(format!("{name}{suffix}"));
        }
        Ok(out)
    }

    /// Writes a file, creating parent directories within the workspace.
    pub fn write(&self, path: &str, content: &[u8]) -> Result<(), String> {
        let full = self.writable_path(path)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("can't create {}: {error}", parent.display()))?;
        }
        std::fs::write(&full, content).map_err(|error| format!("can't write {path}: {error}"))
    }

    /// Deletes a file.
    pub fn delete(&self, path: &str) -> Result<(), String> {
        let full = self.writable_path(path)?;
        if full.is_dir() {
            return Err(format!("{path} is a directory; only files can be deleted"));
        }
        std::fs::remove_file(&full).map_err(|error| format!("can't delete {path}: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_way_of_writing_a_path_is_located_where_it_really_is() {
        let (dir, ws) = workspace();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "a").unwrap();
        assert_eq!(ws.locate("src/a.rs").unwrap(), Location::Inside("src/a.rs".into()));
        assert_eq!(ws.locate("./src/../src/a.rs").unwrap(), Location::Inside("src/a.rs".into()));
        assert_eq!(ws.locate(&root.join("src/a.rs").to_string_lossy()).unwrap(), Location::Inside("src/a.rs".into()));
        assert_eq!(ws.locate(".").unwrap(), Location::Inside(".".into()));
        assert_eq!(ws.locate("new/file.txt").unwrap(), Location::Inside("new/file.txt".into()), "not yet existing");

        let elsewhere = tempfile::tempdir().unwrap();
        let other = elsewhere.path().canonicalize().unwrap();
        assert_eq!(ws.locate(&other.join("x.md").to_string_lossy()).unwrap(), Location::Outside(other.join("x.md")));
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        assert_eq!(ws.locate("~/x-recode-test.md").unwrap(), Location::Outside(home.join("x-recode-test.md")));
        assert_eq!(ws.locate("..").unwrap(), Location::Outside(root.parent().unwrap().to_path_buf()));
    }

    #[test]
    fn a_symlink_in_the_project_is_located_at_its_real_target() {
        let (dir, ws) = workspace();
        let elsewhere = tempfile::tempdir().unwrap();
        let target = elsewhere.path().canonicalize().unwrap();
        std::fs::write(target.join("secret.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("link")).unwrap();
        // Shown (and approved) as the real place, not as a path inside the project.
        assert_eq!(ws.locate("link/secret.txt").unwrap(), Location::Outside(target.join("secret.txt")));
        // The confined inside path still refuses it, as before.
        assert!(ws.read("link/secret.txt").is_err());
    }

    #[test]
    fn outside_files_can_be_read_listed_written_and_deleted() {
        let (_dir, ws) = workspace();
        let elsewhere = tempfile::tempdir().unwrap();
        let file = ws.locate(&elsewhere.path().join("deep/n.md").to_string_lossy()).unwrap();
        assert!(file.is_outside());
        assert_eq!(ws.read_existing_at(&file).unwrap(), None);
        ws.write_at(&file, b"hi").unwrap();
        assert_eq!(ws.read_at(&file).unwrap(), b"hi");
        let folder = ws.locate(&elsewhere.path().join("deep").to_string_lossy()).unwrap();
        assert_eq!(ws.list_at(&folder).unwrap(), ["n.md"]);
        assert!(ws.delete_at(&folder).unwrap_err().contains("directory"));
        ws.delete_at(&file).unwrap();
        assert!(ws.read_at(&file).unwrap_err().contains("doesn't exist"));
    }

    fn workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\nworld\n").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), "nested").unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn reads_lists_and_writes_within_the_root() {
        let (_dir, ws) = workspace();
        assert_eq!(ws.read("a.txt").unwrap(), b"hello\nworld\n");
        assert_eq!(ws.read("sub/b.txt").unwrap(), b"nested");
        let mut top = ws.list(".").unwrap();
        top.sort();
        assert_eq!(top, ["a.txt", "sub/"]);
        ws.write("new/deep.txt", b"x").unwrap();
        assert_eq!(ws.read("new/deep.txt").unwrap(), b"x");
        ws.delete("a.txt").unwrap();
        assert!(ws.read("a.txt").is_err());
    }

    #[test]
    fn parent_traversal_is_refused() {
        let (_dir, ws) = workspace();
        assert!(ws.read("../secret").unwrap_err().contains("outside"));
        assert!(ws.read("sub/../../etc/passwd").unwrap_err().contains("outside"));
        assert!(ws.read("/etc/passwd").unwrap_err().contains("absolute"));
        assert!(ws.write("../evil", b"x").is_err());
    }

    #[test]
    fn a_symlink_pointing_outside_cannot_be_read() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "top secret").unwrap();
        let (dir, ws) = workspace();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();
        // openat2 BENEATH refuses to follow the escaping symlink.
        assert!(ws.read("link").is_err());
    }

    #[test]
    fn a_symlinked_directory_cannot_redirect_a_write() {
        let outside = tempfile::tempdir().unwrap();
        let (dir, ws) = workspace();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        assert!(ws.write("escape/evil.txt", b"x").unwrap_err().contains("outside"));
        assert!(!outside.path().join("evil.txt").exists());
    }
}
