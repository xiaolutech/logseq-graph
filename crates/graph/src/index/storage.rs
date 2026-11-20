//! Atomic generation selection. Database generations never share WAL or SHM filenames.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use serde_json::json;

use super::{Index, IndexError};

#[derive(Debug)]
pub(super) struct Location {
    pub(super) logical: PathBuf,
    pub(super) actual: PathBuf,
    _lease: File,
}

#[derive(Debug)]
pub(crate) struct Shadow {
    index: Option<Index>,
    logical: PathBuf,
    path: PathBuf,
    _build: File,
}

fn io<T>(path: &Path, result: io::Result<T>) -> Result<T, IndexError> {
    result.map_err(|source| IndexError::Io {
        path: path.to_owned(),
        source,
    })
}

fn invalid(path: &Path, message: &str) -> IndexError {
    IndexError::Io {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidData, message),
    }
}

fn suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn lock_file(path: &Path) -> Result<File, IndexError> {
    regular_or_absent(path)?;
    io(
        path,
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path),
    )
}

fn regular_or_absent(path: &Path) -> Result<(), IndexError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => Err(invalid(
            path,
            "index coordination entries must be regular files, not symlinks",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(IndexError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn prefix(path: &Path) -> Result<String, IndexError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("{name}.shadow."))
        .ok_or_else(|| invalid(path, "shadow index filenames must be UTF-8"))
}

pub(super) fn resolve(path: &Path) -> Result<PathBuf, IndexError> {
    let pointer = suffix(path, ".current");
    regular_or_absent(&pointer)?;
    let file = match File::open(&pointer) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(path.to_owned()),
        Err(source) => {
            return Err(IndexError::Io {
                path: pointer,
                source,
            })
        }
    };
    let mut bytes = Vec::new();
    io(&pointer, file.take(4097).read_to_end(&mut bytes))?;
    if bytes.len() > 4096 {
        return Err(invalid(&pointer, "oversized generation pointer"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid(&pointer, "invalid generation pointer"))?;
    let name = value
        .get("file")
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid(&pointer, "missing generation filename"))?;
    if value.get("version").and_then(|v| v.as_u64()) != Some(1)
        || !name.starts_with(&prefix(path)?)
        || !name.ends_with(".sqlite")
        || Path::new(name).components().count() != 1
        || !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(invalid(&pointer, "invalid generation filename or version"));
    }
    let actual = path.with_file_name(name);
    regular_or_absent(&actual)?;
    if !io(&actual, actual.try_exists())? {
        return Err(invalid(
            &actual,
            "published generation is missing; remove the whole derived cache to rebuild",
        ));
    }
    Ok(actual)
}

impl Location {
    pub(super) fn open(path: &Path) -> Result<Self, IndexError> {
        let logical = io(path, std::path::absolute(path))?;
        let parent = logical
            .parent()
            .ok_or_else(|| invalid(&logical, "index has no parent"))?;
        io(parent, fs::create_dir_all(parent))?;
        if !io(&logical, logical.try_exists())?
            && io(&logical, suffix(&logical, ".current").try_exists())?
        {
            let build = lock_file(&suffix(&logical, ".build.lock"))?;
            match build.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => {
                    return Err(invalid(
                        &logical,
                        "cache removed during an active shadow build; retry opening",
                    ))
                }
                Err(TryLockError::Error(source)) => {
                    return Err(IndexError::Io {
                        path: logical,
                        source,
                    })
                }
            }
            let pointer = suffix(&logical, ".current");
            io(&pointer, fs::remove_file(&pointer))?;
            sync_directory(parent)?;
        }
        cleanup(&logical)?;
        loop {
            let actual = resolve(&logical)?;
            let lease_path = suffix(&actual, ".lease");
            let lease = lock_file(&lease_path)?;
            io(&lease_path, lease.lock_shared())?;
            if resolve(&logical)? == actual {
                return Ok(Self {
                    logical,
                    actual,
                    _lease: lease,
                });
            }
        }
    }
}

fn cleanup(logical: &Path) -> Result<(), IndexError> {
    let lock = lock_file(&suffix(logical, ".build.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(source)) => {
            return Err(IndexError::Io {
                path: logical.to_owned(),
                source,
            })
        }
    }
    let current = resolve(logical)?;
    let prefix = prefix(logical)?;
    let parent = logical
        .parent()
        .ok_or_else(|| invalid(logical, "index has no parent"))?;
    for entry in io(parent, fs::read_dir(parent))? {
        let entry = io(parent, entry)?;
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(&prefix) && name.ends_with(".sqlite.lease") {
            let database = parent.join(name.trim_end_matches(".lease"));
            if !io(&database, database.try_exists())? {
                let lease = lock_file(&path)?;
                if lease.try_lock().is_ok() {
                    drop(lease);
                    let _cleanup = fs::remove_file(&path);
                }
            }
            continue;
        }
        if path == current || !name.starts_with(&prefix) || !name.ends_with(".sqlite") {
            continue;
        }
        let lease = lock_file(&suffix(&path, ".lease"))?;
        match lease.try_lock() {
            Ok(()) => match remove_generation(&path) {
                Ok(()) => {}
                Err(IndexError::Io { source, .. })
                    if source.kind() == io::ErrorKind::PermissionDenied => {}
                Err(error) => return Err(error),
            },
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(source)) => return Err(IndexError::Io { path, source }),
        }
    }
    Ok(())
}

fn remove_generation(path: &Path) -> Result<(), IndexError> {
    for path in [
        path.to_owned(),
        suffix(path, "-wal"),
        suffix(path, "-shm"),
        suffix(path, "-journal"),
        suffix(path, ".current.tmp"),
        suffix(path, ".build.lock"),
        suffix(path, ".lease"),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(IndexError::Io { path, source }),
        }
    }
    Ok(())
}

pub(super) fn sync_directory(path: &Path) -> Result<(), IndexError> {
    #[cfg(unix)]
    io(path, File::open(path).and_then(|file| file.sync_all()))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

impl Index {
    /// The physical SQLite file of this connection, or `None` for an in-memory index.
    ///
    /// File-backed indexes accept a logical path; after a shadow publication its `.current`
    /// sidecar selects this generation-specific path. Direct SQLite inspection must use this path.
    pub fn storage_path(&self) -> Option<&Path> {
        self.location
            .as_ref()
            .map(|location| location.actual.as_path())
    }

    pub(crate) fn reopen_if_published(&mut self) -> Result<bool, IndexError> {
        let Some(location) = &self.location else {
            return Ok(false);
        };
        if !io(&location.logical, location.logical.try_exists())?
            && !io(
                &location.logical,
                suffix(&location.logical, ".current").try_exists(),
            )?
        {
            return Ok(false);
        }
        if io(&location.logical, location.logical.try_exists())?
            && resolve(&location.logical)? == location.actual
        {
            return Ok(false);
        }
        *self = if self.read_only {
            Self::open_reader(&location.logical)?
        } else {
            Self::open_at(&location.logical)?
        };
        Ok(true)
    }

    pub(crate) fn shadow(&mut self) -> Result<Shadow, IndexError> {
        let logical = self
            .location
            .as_ref()
            .ok_or_else(|| invalid(Path::new(""), "shadow requires a file-backed index"))?
            .logical
            .clone();
        let build_path = suffix(&logical, ".build.lock");
        let build = lock_file(&build_path)?;
        io(&build_path, build.lock())?;
        self.reopen_if_published()?;
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|source| invalid(&logical, &source.to_string()))?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = logical.with_file_name(format!("{}{token}.sqlite", prefix(&logical)?));
        io(
            &path,
            OpenOptions::new().write(true).create_new(true).open(&path),
        )?;
        let mut shadow = Shadow {
            index: None,
            logical,
            path,
            _build: build,
        };
        let filename = shadow
            .path
            .to_str()
            .ok_or_else(|| invalid(&shadow.path, "shadow paths must be UTF-8"))?;
        self.conn.execute("VACUUM INTO ?1", [filename])?;
        let index = Self::open_at_once(&shadow.path)?;
        index.conn.pragma_update(None, "journal_mode", "DELETE")?;
        index.conn.pragma_update(None, "synchronous", "FULL")?;
        shadow.index = Some(index);
        Ok(shadow)
    }
}

impl Shadow {
    pub(crate) fn database(&mut self) -> Result<&mut Index, IndexError> {
        self.index
            .as_mut()
            .ok_or_else(|| invalid(&self.path, "shadow database is closed"))
    }

    pub(crate) fn sync(&self) -> Result<(), IndexError> {
        io(
            &self.path,
            File::open(&self.path).and_then(|file| file.sync_all()),
        )?;
        if let Some(parent) = self.path.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }

    pub(crate) fn publish(mut self) -> Result<Index, IndexError> {
        let mut index = self
            .index
            .take()
            .ok_or_else(|| invalid(&self.path, "shadow database is closed"))?;
        index.conn.pragma_update(None, "journal_mode", "WAL")?;
        index.conn.pragma_update(None, "synchronous", "NORMAL")?;
        let _ = index.metadata()?;
        self.sync()?;
        let pointer = suffix(&self.logical, ".current");
        regular_or_absent(&pointer)?;
        let temporary = suffix(&self.path, ".current.tmp");
        let mut file = io(
            &temporary,
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary),
        )?;
        let value =
            json!({"version":1, "file":self.path.file_name().and_then(|name| name.to_str())});
        io(&temporary, file.write_all(value.to_string().as_bytes()))?;
        io(&temporary, file.sync_all())?;
        drop(file);
        io(&pointer, fs::rename(&temporary, &pointer))?;
        if let Some(parent) = self.logical.parent() {
            sync_directory(parent)?;
        }
        index.location = Some(Location::open(&self.logical)?);
        Ok(index)
    }
}

impl Drop for Shadow {
    fn drop(&mut self) {
        drop(self.index.take());
        if resolve(&self.logical).is_ok_and(|current| current != self.path) {
            let _cleanup = remove_generation(&self.path);
        }
    }
}

/// Bytes one file-backed index occupies on disk, split by the role each database file plays.
///
/// Every figure sums a database file and its SQLite sidecars (`-wal`, `-shm`, `-journal`). The
/// coordination files (`.current`, `.lease`, `.build.lock`) are a few bytes each and are not
/// counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageUsage {
    live: u64,
    building: u64,
    retired: u64,
}

impl StorageUsage {
    /// The generation the `.current` pointer selects (or the logical file when there is none):
    /// what a reader opening the index now would read.
    pub fn live(&self) -> u64 {
        self.live
    }

    /// Unpublished shadow generations while a shadow build holds the build lock.
    pub fn building(&self) -> u64 {
        self.building
    }

    /// Every other generation still on disk: the logical file once a pointer selects another
    /// generation, generations replaced by a later publication (still leased by an open reader or
    /// waiting for the next open's cleanup), and shadows a crashed build left behind.
    pub fn retired(&self) -> u64 {
        self.retired
    }
}

fn database_bytes(path: &Path) -> Result<u64, IndexError> {
    let mut total = 0;
    for path in [
        path.to_owned(),
        suffix(path, "-wal"),
        suffix(path, "-shm"),
        suffix(path, "-journal"),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => total += metadata.len(),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(IndexError::Io { path, source }),
        }
    }
    Ok(total)
}

fn build_in_progress(logical: &Path) -> Result<bool, IndexError> {
    let path = suffix(logical, ".build.lock");
    regular_or_absent(&path)?;
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(IndexError::Io { path, source }),
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(source)) => Err(IndexError::Io { path, source }),
    }
}

fn usage(logical: &Path) -> Result<StorageUsage, IndexError> {
    let current = resolve(logical)?;
    let building = build_in_progress(logical)?;
    let mut usage = StorageUsage {
        live: database_bytes(&current)?,
        ..StorageUsage::default()
    };
    if current != logical {
        usage.retired += database_bytes(logical)?;
    }
    let prefix = prefix(logical)?;
    let parent = logical
        .parent()
        .ok_or_else(|| invalid(logical, "index has no parent"))?;
    for entry in io(parent, fs::read_dir(parent))? {
        let path = io(parent, entry)?.path();
        let is_generation = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".sqlite"));
        if !is_generation || path == current {
            continue;
        }
        let bytes = database_bytes(&path)?;
        // A published generation gained a lease when it was opened; a shadow under construction
        // has none until its publication.
        if building && !io(&path, suffix(&path, ".lease").try_exists())? {
            usage.building += bytes;
        } else {
            usage.retired += bytes;
        }
    }
    Ok(usage)
}

impl Index {
    /// Physical disk usage of this index, or `None` for an in-memory index.
    ///
    /// Resolved from the directory as it is now, not from this connection's generation, so it
    /// answers for the generation the next reader opens even before this handle reopens it.
    pub fn storage_usage(&self) -> Result<Option<StorageUsage>, IndexError> {
        self.location
            .as_ref()
            .map(|location| usage(&location.logical))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_shadow_cleanup_respects_builds_and_open_generation_leases() {
        let directory = std::env::temp_dir().join(format!(
            "graph-shadow-cleanup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("directory");
        let path = directory.join("index.db");
        let mut index = Index::open_at(&path).expect("live index");
        let shadow = index.shadow().expect("build");
        let shadow_path = shadow.path.clone();
        let observer = Index::open_at(&path).expect("open during build");
        assert!(shadow_path.exists(), "an active builder is not garbage");
        drop(observer);
        drop(shadow);
        assert!(!shadow_path.exists(), "ordinary abort cleans its shadow");
        assert!(!suffix(&shadow_path, ".lease").exists());
        assert!(!suffix(&shadow_path, ".build.lock").exists());
        fs::write(&shadow_path, "interrupted shadow").expect("leftover");
        fs::write(suffix(&shadow_path, "-journal"), "interrupted journal").expect("journal");
        let observer = Index::open_at(&path).expect("reopen cleans crashed build");
        assert!(!shadow_path.exists());
        assert!(!suffix(&shadow_path, "-journal").exists());
        let mut shadow = index.shadow().expect("new build");
        shadow
            .database()
            .expect("database")
            .finish_shadow(&index, "test")
            .expect("revision");
        let published_path = shadow.path.clone();
        let published = shadow.publish().expect("publish");
        assert_eq!(published.storage_path(), Some(published_path.as_path()));
        drop(published);
        drop(observer);
        drop(index);
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_or_traversing_pointer_is_refused() {
        let directory =
            std::env::temp_dir().join(format!("graph-shadow-pointer-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("directory");
        let path = directory.join("index.db");
        let index = Index::open_at(&path).expect("index");
        let pointer = suffix(&path, ".current");
        fs::write(&pointer, r#"{"version":1,"file":"../escape.sqlite"}"#).expect("pointer");
        assert!(Index::open_at(&path).is_err());
        fs::write(&pointer, vec![b'x'; 4097]).expect("oversized pointer");
        assert!(Index::open_at(&path).is_err());
        fs::remove_file(&pointer).expect("remove pointer");
        std::os::unix::fs::symlink(directory.join("outside"), &pointer).expect("symlink");
        assert!(Index::open_at(&path).is_err());
        drop(index);
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn storage_usage_separates_the_live_generation_from_shadows_and_retired_ones() {
        let directory = std::env::temp_dir().join(format!(
            "graph-shadow-usage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("directory");
        let path = directory.join("index.db");
        let mut index = Index::open_at(&path).expect("live index");
        let base = database_bytes(&path).expect("base bytes");
        assert!(
            fs::metadata(suffix(&path, "-wal")).is_ok(),
            "WAL sidecar exists"
        );
        assert!(
            base > fs::metadata(&path).expect("base").len(),
            "sidecars count"
        );
        let usage = index.storage_usage().expect("usage").expect("file-backed");
        assert_eq!(
            (usage.live(), usage.building(), usage.retired()),
            (base, 0, 0)
        );

        let mut shadow = index.shadow().expect("build");
        let building = index.storage_usage().expect("usage").expect("file-backed");
        assert_eq!(building.live(), database_bytes(&path).expect("base"));
        assert_eq!(
            building.building(),
            database_bytes(&shadow.path).expect("shadow")
        );
        assert!(building.building() > 0);
        assert_eq!(building.retired(), 0);

        shadow
            .database()
            .expect("database")
            .finish_shadow(&index, "test")
            .expect("revision");
        let published_path = shadow.path.clone();
        let published = shadow.publish().expect("publish");
        let after = published
            .storage_usage()
            .expect("usage")
            .expect("file-backed");
        assert_eq!(after.live(), database_bytes(&published_path).expect("live"));
        assert_eq!(after.building(), 0);
        assert_eq!(
            after.retired(),
            database_bytes(&path).expect("logical base stays on disk")
        );
        assert!(Index::open_in_memory()
            .expect("memory")
            .storage_usage()
            .expect("usage")
            .is_none());
        drop(published);
        drop(index);
        fs::remove_dir_all(directory).expect("cleanup");
    }
}
