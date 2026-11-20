//! A cross-process write lock for one graph.
//!
//! [`WriteLock::acquire`] takes an exclusive lock on `write.lock` inside the directory it is given,
//! so two writers — two handles in this process or two processes — cannot hold it at the same time.
//! A writer serialises against other writers by holding the value for as long as its writes last.
//!
//! The lock file is never removed, and dropping a [`WriteLock`] only releases the lock. Deleting
//! the file would reintroduce the classic race: a writer holds the lock on inode X, another writer
//! deletes X and creates inode Y, and from then on both believe they hold the lock. The file is a
//! handle, not the lock's state; a leftover empty `write.lock` means "nobody holds the lock now"
//! and nothing else.
//!
//! The directory is the caller's choice. This layer does not know what "the index directory" is:
//! it takes a directory, uses the fixed file name [`LOCK_FILE_NAME`], and reports a missing
//! directory as [`LockError::Io`] instead of creating it, because what exists is the caller's
//! decision.
//!
//! The granularity is the whole graph, not one page. A per-page lock needs a mapping from page
//! names to file names — case folding, escaping `/`, the formatted journal names — and the write it
//! would protect takes milliseconds, so that mapping and its failure modes are not worth it.
//!
//! The kernel releases the lock when the process dies, so a crashed writer cannot leave a lock
//! behind for good. That is the main reason this uses [`File::try_lock`] rather than creating the
//! file with `O_EXCL` and recording a pid to steal the lock from after a timeout.
//!
//! `wait` is a retry loop — `try_lock`, then [`thread::sleep`] for at most [`RETRY_INTERVAL`] of the
//! remaining budget — with no thread spawned. `None` and `Some(Duration::ZERO)` fail immediately
//! when the lock is held; `Some(d)` keeps retrying for at most `d` and then reports
//! [`LockError::Busy`].
//!
//! Cross-process exclusion is covered by a test that starts this test binary as a child process and
//! waits for it to report that it holds the lock, rather than racing on a sleep.

use std::fmt;
use std::fs::{self, File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// Name of the lock file inside the directory the caller passes.
pub(crate) const LOCK_FILE_NAME: &str = "write.lock";

/// Longest single pause between two `try_lock` attempts.
///
/// A writer releases the lock within milliseconds of finishing, so a coarse interval keeps the
/// number of syscalls low without a caller noticing the delay; a shorter remaining budget is slept
/// in full instead of overshooting it.
pub(crate) const RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// An exclusive lock that serialises writes across processes and handles.
///
/// Holding the value holds the lock; dropping it releases the lock and leaves the file in place.
#[derive(Debug)]
pub struct WriteLock {
    file: File,
    path: PathBuf,
}

pub(crate) enum ScanLock {
    Held(WriteLock),
    Unavailable(LockError),
}

/// Why a [`WriteLock`] was not acquired.
#[derive(Debug)]
pub enum LockError {
    /// The lock file could not be opened, or the kernel refused the lock operation.
    Io(io::Error),
    /// Another handle held the lock for the whole budget.
    ///
    /// `waited` is how long this call kept retrying before giving up; it is near zero for
    /// `wait: None`.
    Busy {
        /// How long this call kept retrying before giving up; near zero when no wait budget was set.
        waited: Duration,
    },
}

impl fmt::Display for LockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(formatter, "cannot open the write lock: {source}"),
            Self::Busy { waited } => write!(
                formatter,
                "another writer holds the write lock (waited {} ms)",
                waited.as_millis()
            ),
        }
    }
}

impl std::error::Error for LockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::Busy { .. } => None,
        }
    }
}

impl WriteLock {
    /// Acquire the write lock in `dir`.
    ///
    /// `wait` is how long to keep retrying: `None` fails immediately when the lock is held,
    /// `Some(d)` retries for at most `d`. `Some(Duration::ZERO)` is the same as `None`.
    ///
    /// `dir` must exist; a missing directory is an error rather than something this call creates.
    /// The lock file is created when it is absent and reused when it is present, and it is left on
    /// disk when the returned value is dropped.
    pub fn acquire(dir: &Path, wait: Option<Duration>) -> Result<Self, LockError> {
        let target = lock_target(dir)?;
        lock_exists(&target)?;
        let file = open_for_write(&target).map_err(LockError::Io)?;
        Self::lock(file, dir.join(LOCK_FILE_NAME), wait)
    }

    /// Exclude writers during index work, even if an existing lock must be opened read-only.
    ///
    /// An absent lock that cannot be created also prevents cooperating writers from starting.
    /// Only that case allows index work without a lock; contention and other errors still fail.
    pub(crate) fn acquire_for_scan(
        dir: &Path,
        wait: Option<Duration>,
    ) -> Result<ScanLock, LockError> {
        let target = lock_target(dir)?;
        lock_exists(&target)?;
        let file = match open_for_write(&target) {
            Ok(file) => file,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
                ) =>
            {
                if !lock_exists(&target)? {
                    return Ok(ScanLock::Unavailable(LockError::Io(error)));
                }
                File::open(&target).map_err(LockError::Io)?
            }
            Err(error) => return Err(LockError::Io(error)),
        };
        Self::lock(file, dir.join(LOCK_FILE_NAME), wait).map(ScanLock::Held)
    }

    fn lock(file: File, path: PathBuf, wait: Option<Duration>) -> Result<Self, LockError> {
        let started = Instant::now();
        let budget = wait.filter(|duration| !duration.is_zero());
        loop {
            match file.try_lock() {
                Ok(()) => {
                    return Ok(Self { file, path });
                }
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(source)) => return Err(LockError::Io(source)),
            }

            // The lock is held elsewhere. Without a budget that is the answer; with one, sleep off
            // part of what is left and try again. Elapsed time is measured rather than counted down
            // so a sleep that returns early still ends at the budget instead of before it.
            let Some(budget) = budget else {
                return Err(LockError::Busy {
                    waited: started.elapsed(),
                });
            };
            let elapsed = started.elapsed();
            if elapsed >= budget {
                return Err(LockError::Busy { waited: elapsed });
            }
            thread::sleep(RETRY_INTERVAL.min(budget - elapsed));
        }
    }

    /// The lock file this handle holds.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn lock_target(dir: &Path) -> Result<PathBuf, LockError> {
    Ok(fs::canonicalize(dir)
        .map_err(LockError::Io)?
        .join(LOCK_FILE_NAME))
}

fn lock_exists(target: &Path) -> Result<bool, LockError> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if !metadata.is_file() => Err(LockError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the write lock must be a regular file, not a symlink or directory",
        ))),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(LockError::Io(error)),
    }
}

fn open_for_write(target: &Path) -> io::Result<File> {
    // Reuse an existing lock file without changing its contents.
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(target)
}

impl Drop for WriteLock {
    /// Release the lock by closing the handle, and leave the file on disk.
    ///
    /// Removing the file here would let a second writer create a fresh inode while a third still
    /// holds a lock on the old one, which is exactly the state this lock exists to prevent.
    fn drop(&mut self) {
        // Closing the file releases the lock on every supported platform; unlocking first only
        // makes the release explicit, and a failure here still ends with the handle closed.
        drop(self.file.unlock());
    }
}

/// Where the cross-process write lock directory is chosen.
///
/// [`WriteLock::acquire`] takes a directory as given, because "which directory" is the caller's
/// decision (see the module documentation above). This function is that decision: it maps a graph
/// root onto the one directory every writer of the graph can agree on, and lives
/// here rather than in the engine because it only reads the graph filesystem, which is this layer's
/// boundary (ADR-0003).
///
/// Directory the graph write lock is taken in: `<root>/.git` when the graph root is a Git working
/// tree, and the graph root otherwise. A symlink at `.git` is not a Git directory or pointer.
///
/// The lock has to be one file every writer of this graph agrees on, and `.git` is the only place
/// that is both inside the tree and outside version control:
///
/// - The graph root itself would be the obvious place, but the lock file would then be an untracked
///   file in the user's repository. It would show up in `git status`, and a `git add -A` would commit
///   it.
/// - The index directory, where this used to live, fails both ways. Two handles opened with
///   different `--index` paths take two different locks and never exclude each other, although they
///   write the same files. And the writer that actually loses data is usually not a second instance
///   of this binary but an external tool — git-sync, Syncthing, another editor — working in the same
///   tree; a lock inside our own cache directory is invisible to it, which is the same as no lock.
///   `.git` is a path such a tool can be told to respect, and it sits in the tree the writer touches.
///
/// A `.git` *file* is a linked worktree or a submodule, where Git stores a `gitdir: <path>` pointer
/// instead of a directory. The pointed-to directory is the real Git directory, so the lock goes
/// there. A pointer that cannot be read, or that names a directory that is not there, degrades to the
/// graph root instead of failing the open: a write must not be impossible because a repository
/// is laid out in a way this slice does not follow.
///
/// The fallback is the canonical graph root, independent of the chosen index location.
pub(crate) fn lock_directory(root: &Path) -> PathBuf {
    let dot_git = root.join(".git");
    let metadata = match fs::symlink_metadata(&dot_git) {
        Ok(metadata) => metadata,
        Err(_) => return root.to_owned(),
    };
    if metadata.is_dir() {
        return dot_git;
    }
    if metadata.is_file() {
        if let Some(git_dir) = git_directory_pointer(&dot_git, root) {
            return git_dir;
        }
    }
    root.to_owned()
}

/// Resolves the `gitdir: <path>` pointer a linked worktree or a submodule keeps in its `.git` file.
///
/// Only the first line is read, and a relative path is resolved against `root`, which is where Git
/// writes it. The result must be an existing directory, because the caller takes the lock inside it:
/// a pointer at a file, or at nothing, is treated as unreadable so the caller falls back.
fn git_directory_pointer(pointer: &Path, root: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(pointer).ok()?;
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let target = Path::new(target);
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        root.join(target)
    };
    resolved.is_dir().then_some(resolved)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// Environment variable that turns the child test from a no-op into the lock holder.
    const CHILD_FLAG: &str = "LG_LOCK_CHILD";
    /// Directory the child process locks, passed from the parent.
    const CHILD_DIR: &str = "LG_LOCK_CHILD_DIR";

    /// This module's path as libtest registers it, i.e. without the leading crate name.
    fn test_module_path() -> String {
        module_path!()
            .split_once("::")
            .map_or_else(|| module_path!().to_owned(), |(_, rest)| rest.to_owned())
    }

    /// A temporary directory that removes itself when the test ends.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "logseq-graph-lock-{}-{label}-{serial}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create the test directory");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn a_second_acquire_in_the_same_process_is_busy() {
        let dir = TempDir::new("same-process");
        let held = WriteLock::acquire(&dir.path, None).expect("acquire the first lock");

        match WriteLock::acquire(&dir.path, None) {
            Err(LockError::Busy { .. }) => {}
            other => panic!("expected Busy while the lock is held, got {other:?}"),
        }

        drop(held);
        WriteLock::acquire(&dir.path, None).expect("acquire after the first lock is released");
    }

    #[test]
    fn locks_in_different_directories_do_not_conflict() {
        let first = TempDir::new("first");
        let second = TempDir::new("second");

        let a = WriteLock::acquire(&first.path, None).expect("acquire in the first directory");
        let b = WriteLock::acquire(&second.path, None).expect("acquire in the second directory");

        assert_ne!(a.path(), b.path());
        assert_eq!(a.path().file_name().unwrap(), LOCK_FILE_NAME);
    }

    #[test]
    fn the_lock_file_survives_the_lock() {
        let dir = TempDir::new("survives");
        let path = dir.path.join(LOCK_FILE_NAME);

        {
            let held = WriteLock::acquire(&dir.path, None).expect("acquire the lock");
            assert_eq!(held.path(), path);
            assert!(path.exists());
        }

        // The file is a handle, not the lock's state: releasing the lock must not remove it.
        assert!(path.exists());
    }

    #[test]
    fn a_missing_directory_is_an_error_and_is_not_created() {
        let dir = TempDir::new("missing");
        let absent = dir.path.join("absent");

        match WriteLock::acquire(&absent, None) {
            Err(LockError::Io(_)) => {}
            other => panic!("expected Io for a missing directory, got {other:?}"),
        }
        assert!(
            !absent.exists(),
            "the missing directory must not be created"
        );
    }

    #[cfg(unix)]
    #[test]
    fn lock_files_never_follow_symlinks() {
        let dir = TempDir::new("symlink-lock");
        let outside = TempDir::new("symlink-target");
        let target = outside.path.join("target");
        let link = dir.path.join(LOCK_FILE_NAME);
        std::os::unix::fs::symlink(&target, &link).expect("dangling lock symlink");
        assert!(matches!(
            WriteLock::acquire(&dir.path, None),
            Err(LockError::Io(_))
        ));
        assert!(!target.exists());
        fs::write(&target, "untouched").expect("existing target");
        assert!(matches!(
            WriteLock::acquire(&dir.path, None),
            Err(LockError::Io(_))
        ));
        assert_eq!(
            fs::read_to_string(&target).expect("target bytes"),
            "untouched"
        );
        fs::remove_file(&link).expect("remove link");
        WriteLock::acquire(&dir.path, None).expect("ordinary lock");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_dot_git_does_not_redirect_the_graph_lock() {
        let dir = TempDir::new("symlink-git");
        let outside = TempDir::new("symlink-git-target");
        std::os::unix::fs::symlink(&outside.path, dir.path.join(".git"))
            .expect("symlink git directory");
        assert_eq!(lock_directory(&dir.path), dir.path);
        let lock = WriteLock::acquire(&lock_directory(&dir.path), None).expect("root lock");
        assert_eq!(lock.path(), dir.path.join(LOCK_FILE_NAME));
        assert!(!outside.path.join(LOCK_FILE_NAME).exists());
    }

    #[test]
    fn a_budget_keeps_retrying_until_it_is_spent() {
        let dir = TempDir::new("budget");
        let held = WriteLock::acquire(&dir.path, None).expect("acquire the first lock");
        let budget = Duration::from_millis(20);

        let started = Instant::now();
        let outcome = WriteLock::acquire(&dir.path, Some(budget));
        let elapsed = started.elapsed();

        match outcome {
            Err(LockError::Busy { .. }) => {}
            other => panic!("expected Busy while the lock is held, got {other:?}"),
        }
        assert!(
            elapsed >= budget,
            "a {budget:?} budget must be waited out, but the call took {elapsed:?}"
        );
        drop(held);
    }

    #[test]
    fn a_zero_budget_does_not_wait() {
        let dir = TempDir::new("zero-budget");
        let held = WriteLock::acquire(&dir.path, None).expect("acquire the first lock");

        match WriteLock::acquire(&dir.path, Some(Duration::ZERO)) {
            Err(LockError::Busy { .. }) => {}
            other => panic!("expected Busy for a zero budget, got {other:?}"),
        }
        drop(held);
    }

    /// The child half of [`another_process_cannot_take_the_lock`].
    ///
    /// Without the environment variable this is a no-op, so the ordinary test run passes it
    /// trivially; the parent runs exactly this test in a child process with the variable set.
    #[test]
    fn child_holds_the_lock() {
        if std::env::var_os(CHILD_FLAG).is_none() {
            return;
        }
        let dir = std::env::var_os(CHILD_DIR).expect("the parent passes a directory");
        let held = WriteLock::acquire(Path::new(&dir), None).expect("the child acquires the lock");
        // The parent uses this line as its synchronisation point, so it must arrive only after the
        // lock is held.
        println!("locked {}", held.path().display());
        thread::sleep(Duration::from_secs(2));
    }

    #[test]
    fn another_process_cannot_take_the_lock() {
        let dir = TempDir::new("cross-process");
        // libtest registers a unit test under its module path without the crate name, and prints a
        // preamble of its own before the test runs, so the report is read by content, not position.
        let child_test = format!("{}::child_holds_the_lock", test_module_path());
        let mut child = Command::new(std::env::current_exe().expect("the test binary path"))
            .arg("--exact")
            .arg(&child_test)
            .arg("--nocapture")
            .env(CHILD_FLAG, "1")
            .env(CHILD_DIR, &dir.path)
            .stdout(Stdio::piped())
            .spawn()
            .expect("start the child test process");

        let stdout = child.stdout.take().expect("the child's stdout pipe");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let mut reported = false;
        for _ in 0..10 {
            line.clear();
            let read = reader
                .read_line(&mut line)
                .expect("read the child's output");
            if read == 0 {
                break;
            }
            if line.starts_with("locked ") {
                reported = true;
                break;
            }
        }
        assert!(
            reported,
            "the child must report the held lock, last line was {line:?}"
        );

        let outcome = WriteLock::acquire(&dir.path, None);
        let _ = child.kill();
        let _ = child.wait();

        match outcome {
            Err(LockError::Busy { .. }) => {}
            other => panic!("expected Busy while another process holds the lock, got {other:?}"),
        }

        // The kernel drops the dead process's lock, so nothing is left to steal or time out.
        WriteLock::acquire(&dir.path, Some(Duration::from_secs(1)))
            .expect("acquire after the holding process is gone");
    }
}
