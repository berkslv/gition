//! File access for the app. The terminal build works on a directory on disk,
//! the web build on an in-memory working copy of a GitHub repo.
//!
//! All paths are repo-relative; the empty path is the root.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

pub trait Storage {
    /// Display name of the root.
    fn name(&self) -> String;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Writes a file, creating its parent directories.
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;
    fn exists(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    /// Moves a file or directory, creating the target's parent directories.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Removes a file, or a directory and everything in it.
    fn remove(&self, path: &Path) -> io::Result<()>;

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        String::from_utf8(self.read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

impl<T: Storage + ?Sized> Storage for Rc<T> {
    fn name(&self) -> String {
        (**self).name()
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        (**self).read(path)
    }
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        (**self).write(path, data)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        (**self).read_dir(path)
    }
    fn exists(&self, path: &Path) -> bool {
        (**self).exists(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        (**self).is_dir(path)
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        (**self).create_dir_all(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        (**self).rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        (**self).remove(path)
    }
}

/// A directory on disk.
pub struct FsStorage {
    pub root: PathBuf,
}

impl FsStorage {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Storage for FsStorage {
    fn name(&self) -> String {
        self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(self.root.join(path))
    }
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let abs = self.root.join(path);
        if let Some(dir) = abs.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(abs, data)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        Ok(std::fs::read_dir(self.root.join(path))?
            .flatten()
            .filter_map(|e| {
                Some(DirEntry {
                    name: e.file_name().to_string_lossy().into_owned(),
                    is_dir: e.file_type().ok()?.is_dir(),
                })
            })
            .collect())
    }
    fn exists(&self, path: &Path) -> bool {
        self.root.join(path).exists()
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.root.join(path).is_dir()
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(self.root.join(path))
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let to = self.root.join(to);
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::rename(self.root.join(from), to)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        let abs = self.root.join(path);
        if abs.is_dir() { std::fs::remove_dir_all(abs) } else { std::fs::remove_file(abs) }
    }
}

/// File content in a [`MemStorage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blob {
    Data(Vec<u8>),
    /// Not downloaded; the git blob SHA it can be fetched by.
    Remote(String),
}

pub type Files = BTreeMap<PathBuf, Blob>;

#[derive(Default)]
struct Mem {
    files: Files,
    /// Directories with no files in them yet.
    dirs: BTreeSet<PathBuf>,
    version: u64,
}

impl Mem {
    fn is_dir(&self, path: &Path) -> bool {
        path.as_os_str().is_empty()
            || self.dirs.iter().any(|d| d.starts_with(path))
            || self.files.keys().any(|f| f.starts_with(path) && f != path)
    }

    fn touch(&mut self) {
        self.version += 1;
    }
}

/// An in-memory working copy. Directories exist implicitly while they hold files.
pub struct MemStorage {
    name: String,
    mem: RefCell<Mem>,
}

impl MemStorage {
    pub fn new(name: impl Into<String>, files: Files) -> Self {
        Self { name: name.into(), mem: RefCell::new(Mem { files, ..Mem::default() }) }
    }

    pub fn files(&self) -> Files {
        self.mem.borrow().files.clone()
    }

    pub fn replace(&self, files: Files) {
        let mut m = self.mem.borrow_mut();
        m.files = files;
        m.dirs.clear();
        m.touch();
    }

    /// Changes with every write, so callers can tell when to persist.
    pub fn version(&self) -> u64 {
        self.mem.borrow().version
    }

    fn not_found(path: &Path) -> io::Error {
        io::Error::new(io::ErrorKind::NotFound, format!("{} not found", crate::paths::to_slash(path)))
    }
}

impl Storage for MemStorage {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        match self.mem.borrow().files.get(path) {
            Some(Blob::Data(d)) => Ok(d.clone()),
            Some(Blob::Remote(_)) => Err(io::Error::other("not downloaded")),
            None => Err(Self::not_found(path)),
        }
    }

    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut m = self.mem.borrow_mut();
        if m.is_dir(path) {
            return Err(io::Error::other(format!("{} is a directory", crate::paths::to_slash(path))));
        }
        m.files.insert(path.to_path_buf(), Blob::Data(data.to_vec()));
        m.touch();
        Ok(())
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let m = self.mem.borrow();
        if !m.is_dir(path) {
            return Err(Self::not_found(path));
        }
        let mut out: BTreeMap<String, bool> = BTreeMap::new();
        let files = m.files.keys().map(|p| (p, false));
        let dirs = m.dirs.iter().map(|p| (p, true));
        for (p, is_dir) in files.chain(dirs) {
            let Ok(rest) = p.strip_prefix(path) else { continue };
            let mut parts = rest.components();
            let Some(first) = parts.next() else { continue };
            let name = first.as_os_str().to_string_lossy().into_owned();
            *out.entry(name).or_default() |= is_dir || parts.next().is_some();
        }
        Ok(out.into_iter().map(|(name, is_dir)| DirEntry { name, is_dir }).collect())
    }

    fn exists(&self, path: &Path) -> bool {
        let m = self.mem.borrow();
        m.files.contains_key(path) || m.is_dir(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.mem.borrow().is_dir(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        let mut m = self.mem.borrow_mut();
        if !m.is_dir(path) {
            m.dirs.insert(path.to_path_buf());
            m.touch();
        }
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut m = self.mem.borrow_mut();
        if let Some(blob) = m.files.remove(from) {
            m.files.insert(to.to_path_buf(), blob);
        } else if m.is_dir(from) && !from.as_os_str().is_empty() {
            let moved: Vec<PathBuf> = m.files.keys().filter(|p| p.starts_with(from)).cloned().collect();
            for p in moved {
                let blob = m.files.remove(&p).expect("listed above");
                m.files.insert(to.join(p.strip_prefix(from).expect("prefix checked")), blob);
            }
            let dirs: Vec<PathBuf> = m.dirs.iter().filter(|p| p.starts_with(from)).cloned().collect();
            for d in dirs {
                m.dirs.remove(&d);
                m.dirs.insert(to.join(d.strip_prefix(from).expect("prefix checked")));
            }
        } else {
            return Err(Self::not_found(from));
        }
        m.touch();
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        let mut m = self.mem.borrow_mut();
        if m.files.remove(path).is_none() {
            if !m.is_dir(path) {
                return Err(Self::not_found(path));
            }
            m.files.retain(|p, _| !p.starts_with(path));
            m.dirs.retain(|p| !p.starts_with(path));
        }
        m.touch();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(s: &str) -> Blob {
        Blob::Data(s.as_bytes().to_vec())
    }

    #[test]
    fn mem_storage_behaves_like_a_directory() {
        let mut files = Files::new();
        files.insert("INDEX.md".into(), data("# Home"));
        files.insert("Projects/plan.md".into(), data("plan"));
        files.insert("assets/x.png".into(), Blob::Remote("abc".into()));
        let fs = MemStorage::new("notes", files);

        let names =
            |p: &str| fs.read_dir(Path::new(p)).unwrap().into_iter().map(|e| (e.name, e.is_dir)).collect::<Vec<_>>();
        assert_eq!(
            names(""),
            [("INDEX.md".to_string(), false), ("Projects".to_string(), true), ("assets".to_string(), true)]
        );
        assert!(fs.is_dir(Path::new("Projects")));
        assert!(fs.read(Path::new("assets/x.png")).is_err());

        fs.rename(Path::new("Projects"), Path::new("Archive/Old")).unwrap();
        assert_eq!(fs.read_to_string(Path::new("Archive/Old/plan.md")).unwrap(), "plan");
        assert!(!fs.exists(Path::new("Projects")));

        fs.create_dir_all(Path::new("Empty")).unwrap();
        assert_eq!(names("Empty"), []);
        fs.remove(Path::new("Archive")).unwrap();
        assert!(!fs.exists(Path::new("Archive/Old/plan.md")));
        assert!(fs.write(Path::new("Empty"), b"x").is_err());
    }
}
