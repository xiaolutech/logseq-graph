use std::path::PathBuf;

use super::BlockId;

/// The pending cross-page block move, stored in the graph's durable journal.
///
/// This is the first write intent under ADR-0004: one logical write is about to touch two files,
/// so file state alone can no longer say what the write was trying to reach. The engine records
/// this before replacing the first file, and clears it durably after the multi-page index commit succeeds.
/// A journal that survives a crash is what lets recovery tell an interrupted move from an unrelated
/// edit, and every field it holds is what that comparison needs.
///
/// The fingerprints are the same content fingerprint a file baseline stores; no new hash was
/// introduced. Relative paths are graph-relative, like the paths of
/// the file baseline, and the page names are as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveIntent {
    /// The moved block, addressed in the source pre bytes.
    pub moved: MoveBlockAddress,
    /// The anchor, addressed in the destination pre bytes; recovery does not resolve it.
    pub anchor: MoveBlockAddress,
    /// Where the moved block ends up relative to the anchor block.
    pub anchor_kind: MoveAnchorKind,
    /// Graph-relative path of the file the block moves out of.
    pub from_relative_path: PathBuf,
    /// Name of the page the block moves out of, as written.
    pub from_page: String,
    /// Fingerprint of the source file's bytes before the edit.
    pub from_pre_hash: u64,
    /// Fingerprint of the source file's candidate bytes after the edit.
    pub from_post_hash: u64,
    /// Graph-relative path of the file the block moves into.
    pub to_relative_path: PathBuf,
    /// Name of the page the block moves into, as written.
    pub to_page: String,
    /// Fingerprint of the destination file's bytes before the edit.
    pub to_pre_hash: u64,
    /// Fingerprint of the destination file's candidate bytes after the edit.
    pub to_post_hash: u64,
}

/// A durable identity or a coordinate bound to an intent's corresponding pre fingerprint.
///
/// A path is a child-index path, never an ephemeral UUID. Recovery may resolve it only while
/// the file's current fingerprint equals the recorded pre fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveBlockAddress {
    /// The `id::` UUID the file states.
    Durable(BlockId),
    /// Child indices from the page root in the file's pre bytes.
    Path(Vec<u32>),
}

/// Where the moved block ends up relative to its anchor block.
///
/// This mirrors the relation half of the engine's move target, independently of either address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveAnchorKind {
    /// Immediately before the anchor block, as its previous sibling.
    Before,
    /// Immediately after the anchor block, as its next sibling.
    After,
    /// As the first child of the anchor block.
    FirstChildOf,
    /// As the last child of the anchor block, after every existing child.
    LastChildOf,
}

impl MoveAnchorKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
            Self::FirstChildOf => "first_child_of",
            Self::LastChildOf => "last_child_of",
        }
    }

    pub(crate) fn from_str(stored: &str) -> Option<Self> {
        match stored {
            "before" => Some(Self::Before),
            "after" => Some(Self::After),
            "first_child_of" => Some(Self::FirstChildOf),
            "last_child_of" => Some(Self::LastChildOf),
            _ => None,
        }
    }
}
