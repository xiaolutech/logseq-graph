//! Durable, graph-wide recovery state, independent of disposable SQLite projections.

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use super::{file, write, SourceError, WriteLock};
use crate::domain::{BlockId, MoveAnchorKind, MoveBlockAddress, MoveIntent};

const JOURNAL: &str = "write-intent.json";
const MAX_BYTES: u64 = 1024 * 1024;

/// The durable move journal beside a graph's write lock.
///
/// Callers must hold the same graph lock during every read, write and clear. The journal outlives
/// any index and is never replaced with a different pending operation.
#[derive(Debug)]
pub struct MoveJournal {
    directory: PathBuf,
}

impl MoveJournal {
    /// Resolve the journal directory from an acquired graph write lock.
    pub fn new(lock: &WriteLock) -> Result<Self, SourceError> {
        Self::in_directory(lock.path().parent().unwrap_or(Path::new(".")))
    }

    pub(crate) fn in_directory(directory: &Path) -> Result<Self, SourceError> {
        let directory = file::canonicalize(directory)?;
        Ok(Self { directory })
    }

    pub(crate) fn is_present(&self) -> Result<bool, SourceError> {
        let path = self.directory.join(JOURNAL);
        match fs::symlink_metadata(&path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(SourceError::Io { path, source }),
        }
    }

    fn path(&self) -> Result<PathBuf, SourceError> {
        let path = self.directory.join(JOURNAL);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(invalid(
                    &path,
                    "the move journal must be a regular file, not a symlink or directory",
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(SourceError::Io { path, source }),
        }
        Ok(path)
    }

    /// Read the pending move without spending it. Malformed or oversized journals fail closed.
    pub fn read(&self) -> Result<Option<MoveIntent>, SourceError> {
        let path = self.path()?;
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(SourceError::Io { path, source }),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| SourceError::Io {
                path: path.clone(),
                source,
            })?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid(&path, "move journal exceeds 1 MiB"));
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|error| invalid(&path, error.to_string()))?;
        let text = |key| {
            value
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(&path, format!("missing or invalid {key}")))
        };
        let hash = |key| {
            value
                .get(key)
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid(&path, format!("missing or invalid {key}")))
        };
        if value.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(invalid(&path, "unsupported move journal version"));
        }
        let relative = |key| -> Result<PathBuf, SourceError> {
            let relative = PathBuf::from(text(key)?);
            validate_relative(&path, &relative)?;
            Ok(relative)
        };
        Ok(Some(MoveIntent {
            moved: parse_address(&value, "moved_block_id", "moved_block_path", &path)?,
            anchor: parse_address(&value, "anchor_block_id", "anchor_block_path", &path)?,
            anchor_kind: MoveAnchorKind::from_str(text("anchor_kind")?)
                .ok_or_else(|| invalid(&path, "invalid anchor kind"))?,
            from_relative_path: relative("from_relative_path")?,
            from_page: text("from_page")?.to_owned(),
            from_pre_hash: hash("from_pre_hash")?,
            from_post_hash: hash("from_post_hash")?,
            to_relative_path: relative("to_relative_path")?,
            to_page: text("to_page")?.to_owned(),
            to_pre_hash: hash("to_pre_hash")?,
            to_post_hash: hash("to_post_hash")?,
        }))
    }

    /// Atomically persist a move with file and directory fsync before any page bytes change.
    ///
    /// Republishing the identical move is safe for interrupted migration; a different pending
    /// operation is refused, never overwritten.
    pub fn write(&self, intent: &MoveIntent) -> Result<(), SourceError> {
        let path = self.path()?;
        if let Some(stored) = self.read()? {
            if stored != *intent {
                return Err(invalid(&path, "a different move is already pending"));
            }
            fs::File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(|source| SourceError::Io {
                    path: path.clone(),
                    source,
                })?;
            return write::sync_directory(Some(&self.directory));
        }
        validate_relative(&path, &intent.from_relative_path)?;
        validate_relative(&path, &intent.to_relative_path)?;
        let utf8 = |p: &Path| {
            p.to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid(&path, "move paths must be UTF-8"))
        };
        let mut value = json!({"version": 1, "anchor_kind": intent.anchor_kind.as_str(),
            "from_relative_path": utf8(&intent.from_relative_path)?, "from_page": intent.from_page, "from_pre_hash": intent.from_pre_hash, "from_post_hash": intent.from_post_hash,
            "to_relative_path": utf8(&intent.to_relative_path)?, "to_page": intent.to_page, "to_pre_hash": intent.to_pre_hash, "to_post_hash": intent.to_post_hash});
        for (address, id_key, path_key) in [
            (&intent.moved, "moved_block_id", "moved_block_path"),
            (&intent.anchor, "anchor_block_id", "anchor_block_path"),
        ] {
            match address {
                MoveBlockAddress::Durable(id) => value[id_key] = json!(id.as_str()),
                MoveBlockAddress::Path(indices) => {
                    if indices.is_empty() {
                        return Err(invalid(&path, "move block paths must not be empty"));
                    }
                    value[path_key] = json!(indices);
                }
            }
        }
        let text = value.to_string();
        if text.len() as u64 > MAX_BYTES {
            return Err(invalid(&path, "move journal exceeds 1 MiB"));
        }
        write::write_text(&path, &text).map_err(|error| match error {
            SourceError::Committed { step, source, .. } => SourceError::Io { path: step, source },
            other => other,
        })
    }

    /// Remove a spent journal and fsync its directory. An already absent journal is harmless.
    pub fn clear(&self) -> Result<(), SourceError> {
        let path = self.path()?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(SourceError::Io { path, source }),
        }
        write::sync_directory(Some(&self.directory))
    }
}

fn parse_address(
    value: &Value,
    id_key: &str,
    path_key: &str,
    journal: &Path,
) -> Result<MoveBlockAddress, SourceError> {
    match (value.get(id_key), value.get(path_key)) {
        (Some(id), None) => {
            let id = id
                .as_str()
                .ok_or_else(|| invalid(journal, format!("invalid {id_key}")))?;
            BlockId::parse(id)
                .map(MoveBlockAddress::Durable)
                .map_err(|error| invalid(journal, error.to_string()))
        }
        (None, Some(indices)) => {
            let indices = indices
                .as_array()
                .filter(|indices| !indices.is_empty())
                .ok_or_else(|| invalid(journal, format!("invalid {path_key}")))?;
            let indices = indices
                .iter()
                .map(|index| {
                    index
                        .as_u64()
                        .and_then(|index| u32::try_from(index).ok())
                        .ok_or_else(|| invalid(journal, format!("invalid {path_key} index")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(MoveBlockAddress::Path(indices))
        }
        _ => Err(invalid(
            journal,
            format!("exactly one of {id_key} and {path_key} is required"),
        )),
    }
}

fn invalid(path: &Path, message: impl Into<String>) -> SourceError {
    SourceError::Io {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidData, message.into()),
    }
}

fn validate_relative(journal: &Path, path: &Path) -> Result<(), SourceError> {
    if !path.components().all(|p| matches!(p, Component::Normal(_)))
        || !matches!(path.components().next(), Some(Component::Normal(p)) if p == "pages" || p == "journals")
        || path.components().count() < 2
    {
        return Err(invalid(
            journal,
            "move paths must be plain graph-relative page paths",
        ));
    }
    Ok(())
}
