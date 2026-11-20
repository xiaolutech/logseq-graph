use super::WriteError;
use crate::document::{BlockLocator, PageDocument, PageFacts};
use crate::domain::{BlockId, BlockSelector, BlockView, Read};
use crate::engine::Graph;
use std::time::{SystemTime, UNIX_EPOCH};

/// The identity grant and the block it now names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsuredBlockId {
    /// Bare UUID stored in the block's `id::` property.
    pub id: BlockId,
    /// Whether this call added durable identity to the file.
    pub created: bool,
    /// The block with its durable selector.
    pub block: BlockView,
}

impl Graph {
    /// Explicitly grant durable identity, or answer the identity the file already states.
    ///
    /// This uses the property-write lock, fingerprint gate and commit path. An existing valid
    /// `id::` is returned without replacing the file or advancing the revision. New ids follow
    /// Logseq's file-graph UUID format: Unix seconds in the first eight hex digits of a random v4
    /// UUID, lowercase and hyphenated. No read grants identity implicitly.
    pub fn ensure_block_id(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<Read<EnsuredBlockId>, WriteError> {
        let mut created = false;
        let (block, meta) = self.write_block_property(selector, true, |document, locator| {
            if document.block_durable_id(locator.clone())?.is_some() {
                return Ok(document.to_source());
            }
            let minted = mint_block_id()?;
            let candidate = document.upsert_block_property_text(locator, "id", minted.as_str())?;
            created = true;
            Ok(candidate)
        })?;
        let BlockSelector::Durable(id) = block.selector() else {
            unreachable!("the property write ensured a valid durable id")
        };
        let id = id.clone();
        Ok(Read::new(EnsuredBlockId { id, created, block }, meta))
    }
}

/// Logseq 0.10.9 `src/main/frontend/db.cljs`'s `new-block-id` calls `d/squuid`:
/// <https://github.com/logseq/logseq/blob/0.10.9/src/main/frontend/db.cljs#L198-L200>.
/// DataScript 1.5.3 (Logseq 0.10.9's dependency) `src/datascript/util.cljc`'s `squuid` replaces the first 32 bits of a
/// random v4 UUID with Unix seconds, retaining the v4 version and RFC variant:
/// <https://github.com/tonsky/datascript/blob/1.5.3/src/datascript/util.cljc#L25-L48>.
fn mint_block_id() -> Result<BlockId, WriteError> {
    let failure = |reason: String| WriteError::IdentityGeneration { reason };
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| failure(error.to_string()))?
        .as_secs();
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| failure(error.to_string()))?;
    bytes[..4].copy_from_slice(&seconds.to_be_bytes()[4..]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.map(|byte| format!("{byte:02x}")).concat();
    BlockId::parse(&format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
    .map_err(|error| failure(error.to_string()))
}

pub(super) fn observed_durable_view(
    page: &str,
    document: &PageDocument,
    locator: BlockLocator<'_>,
) -> Result<BlockView, WriteError> {
    let path = document.block_path(locator)?;
    let facts = PageFacts::extract(document);
    let mut blocks = document.blocks();
    let mut preorder = 0u32;
    let mut parent = None;
    let mut selected = None;
    for (depth, index) in path.iter().enumerate() {
        selected = if depth == 0 {
            facts.blocks().get(*index as usize).and_then(|fact| {
                blocks
                    .iter()
                    .find(|block| block.own_span() == fact.own_span())
            })
        } else {
            blocks.get(*index as usize)
        };
        let block = selected.ok_or(crate::document::EditError::PathOutOfRange)?;
        parent = if depth == 0 { None } else { Some(preorder) };
        preorder = facts
            .blocks()
            .iter()
            .position(|fact| fact.own_span() == block.own_span())
            .and_then(|position| u32::try_from(position).ok())
            .ok_or(crate::document::EditError::PathOutOfRange)?;
        blocks = block.children();
    }
    let block = selected.ok_or(crate::document::EditError::PathOutOfRange)?;
    let fact = &facts.blocks()[preorder as usize];
    let id = fact
        .durable_id()
        .ok_or(crate::document::EditError::BlockNotFound)?;
    Ok(BlockView::new(
        page.to_owned(),
        preorder,
        u32::try_from(path.len() - 1).map_err(|error| WriteError::IdentityGeneration {
            reason: error.to_string(),
        })?,
        parent,
        fact.text().to_owned(),
        block.marker(),
        BlockSelector::Durable(id),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squuids_keep_logseq_time_version_variant_and_spelling() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let id = mint_block_id().expect("entropy");
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let text = id.as_str();
        assert_eq!(text.len(), 36);
        assert_eq!(text, text.to_lowercase());
        assert_eq!(&text[14..15], "4");
        assert!(matches!(&text[19..20], "8" | "9" | "a" | "b"));
        let seconds = u64::from_str_radix(&text[..8], 16).expect("hex timestamp");
        assert!((before..=after).contains(&seconds));
        assert_eq!(BlockId::parse(text).expect("UUID"), id);
        assert_ne!(id, mint_block_id().expect("independent entropy"));
    }
}
