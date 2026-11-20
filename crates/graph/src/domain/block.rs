//! Block identity: durable `id::` UUIDs and projection-scoped ephemeral ids (ADR-0005, ADR-0012).
//!
//! A public block target is either durable or ephemeral, never implicitly one of them. Durable
//! identity is a UUID stored in Markdown that survives edits and moves. An ephemeral id is a random
//! UUID the projection mints for a block that states no `id::`; it is stored only in the index, it
//! is never written to a file, and it names one block for as long as its row exists *and* the page's
//! bytes are the ones that row was projected from. SQLite rowids are never public identity.
//!
//! The two kinds are never converted into one another: a block that states an `id::` has no
//! ephemeral id, and a block that states none cannot be given a durable one by any read.

use std::fmt;
use std::str::FromStr;

/// The marker that opens an outliner block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockMarker {
    /// `- `.
    ///
    /// Logseq opens a block on `-` alone; `*` and `+` are valid CommonMark bullets and are text
    /// here. The parser enforces that rule; the variant carries no spelling because the marker is
    /// always `-`.
    Bullet,
    /// One to six `#` characters followed by whitespace.
    Heading {
        /// How many leading `#` characters open the block; values outside `1..=6` are not a heading.
        level: u8,
    },
}

/// Durable block identity: the `id::` UUID stored in a block's Markdown.
///
/// The shape rule is the same one `document::facts` applies to `((ref))` targets: 36 characters,
/// `8-4-4-4-12` hexadecimal with hyphens at positions 8, 13, 18, and 23. The internal spelling is
/// normalized to lowercase, because Logseq and the `((ref))` matcher both compare case-insensitively;
/// two spellings that differ only in case are the same [`BlockId`].
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId {
    value: String,
}

impl BlockId {
    /// Parse a `8-4-4-4-12` hexadecimal UUID, accepting any letter casing.
    pub fn parse(value: &str) -> Result<Self, BlockIdError> {
        Ok(Self {
            value: parse_uuid_shape(value)?,
        })
    }

    /// The canonical lowercase spelling of the UUID.
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl fmt::Debug for BlockId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "BlockId(\"{}\")", self.value)
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}

impl FromStr for BlockId {
    type Err = BlockIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// Failure of [`BlockId::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockIdError {
    /// The input is not 36 characters long.
    WrongLength {
        /// Length of the input in bytes.
        len: usize,
    },
    /// A hyphen position holds another character, or another position holds a non-hex character.
    InvalidCharacter {
        /// Zero-based byte position of the offending character.
        position: usize,
        /// The offending character.
        value: char,
    },
}

impl fmt::Display for BlockIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { len } => write_wrong_length(formatter, *len),
            Self::InvalidCharacter { position, value } => {
                write_invalid_character(formatter, *position, *value)
            }
        }
    }
}

impl std::error::Error for BlockIdError {}

/// Ephemeral block identity: a random UUID minted when a page is projected (ADR-0012).
///
/// It carries no position and no revision, so there is nothing to derive it from and nothing to
/// re-derive: an id names the stored block whose `blocks.ephemeral_id` equals it, and nothing else.
/// That is what makes a stale id fail closed. A revision-bound or position-derived value could be
/// recomputed after an edit and would then name whatever block happens to sit there now; this value
/// cannot be recomputed at all, because the only way to obtain one is to ask the index.
///
/// The id lives in the index and only in the index: no read writes it to Markdown, no write records
/// it in an intent, and it is not durable identity under another spelling. Rebuilding the index, or
/// changing the bytes of the page it was minted for, destroys it.
///
/// The shape is the same `8-4-4-4-12` lowercase hexadecimal UUID as [`BlockId`], and the two types
/// still never convert into one another: sharing a shape is not sharing a meaning.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EphemeralId {
    value: String,
}

impl EphemeralId {
    /// Parse a `8-4-4-4-12` hexadecimal UUID, accepting any letter casing.
    pub fn parse(value: &str) -> Result<Self, EphemeralIdError> {
        Ok(Self {
            value: parse_uuid_shape(value)?,
        })
    }

    /// Canonical id spelling of sixteen random bytes.
    ///
    /// The caller is responsible for the bytes being unpredictable: this only renders them, so the
    /// index layer, which owns the random source, is the only place that can mint one.
    pub(crate) fn from_bytes(bytes: [u8; 16]) -> Self {
        let mut value = String::with_capacity(36);
        for (position, byte) in bytes.iter().enumerate() {
            if matches!(position, 4 | 6 | 8 | 10) {
                value.push('-');
            }
            // Two lowercase hex digits, written without a format allocation.
            const DIGITS: &[u8; 16] = b"0123456789abcdef";
            value.push(DIGITS[usize::from(byte >> 4)] as char);
            value.push(DIGITS[usize::from(byte & 0x0f)] as char);
        }
        Self { value }
    }

    /// The canonical lowercase spelling of the UUID.
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl fmt::Debug for EphemeralId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "EphemeralId(\"{}\")", self.value)
    }
}

impl fmt::Display for EphemeralId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}

impl FromStr for EphemeralId {
    type Err = EphemeralIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// Failure of [`EphemeralId::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EphemeralIdError {
    /// The input is not 36 characters long.
    WrongLength {
        /// Length of the input in bytes.
        len: usize,
    },
    /// A hyphen position holds another character, or another position holds a non-hex character.
    InvalidCharacter {
        /// Zero-based byte position of the offending character.
        position: usize,
        /// The offending character.
        value: char,
    },
}

impl fmt::Display for EphemeralIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { len } => write_wrong_length(formatter, *len),
            Self::InvalidCharacter { position, value } => {
                write_invalid_character(formatter, *position, *value)
            }
        }
    }
}

impl std::error::Error for EphemeralIdError {}

/// Why a string is not a `8-4-4-4-12` hexadecimal UUID.
///
/// Private on purpose: both identity kinds validate the same shape and each reports it through its
/// own error type, so the shape rule exists once without the two identities sharing a public type.
enum UuidShapeError {
    WrongLength { len: usize },
    InvalidCharacter { position: usize, value: char },
}

/// Check the shared shape rule and return the canonical lowercase spelling.
fn parse_uuid_shape(value: &str) -> Result<String, UuidShapeError> {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return Err(UuidShapeError::WrongLength { len: bytes.len() });
    }
    for (position, character) in value.char_indices() {
        let is_hyphen_position = matches!(position, 8 | 13 | 18 | 23);
        let valid = if is_hyphen_position {
            character == '-'
        } else {
            character.is_ascii_hexdigit()
        };
        if !valid {
            return Err(UuidShapeError::InvalidCharacter {
                position,
                value: character,
            });
        }
    }
    Ok(value.to_ascii_lowercase())
}

/// Word the length half of the shape rule; both identity errors report it this way.
fn write_wrong_length(formatter: &mut fmt::Formatter<'_>, len: usize) -> fmt::Result {
    write!(
        formatter,
        "not an 8-4-4-4-12 hexadecimal UUID: {len} characters, expected 36"
    )
}

/// Word the character half of the shape rule.
fn write_invalid_character(
    formatter: &mut fmt::Formatter<'_>,
    position: usize,
    value: char,
) -> fmt::Result {
    write!(
        formatter,
        "not an 8-4-4-4-12 hexadecimal UUID: unexpected {value:?} at position {position}"
    )
}

impl From<UuidShapeError> for BlockIdError {
    fn from(error: UuidShapeError) -> Self {
        match error {
            UuidShapeError::WrongLength { len } => Self::WrongLength { len },
            UuidShapeError::InvalidCharacter { position, value } => {
                Self::InvalidCharacter { position, value }
            }
        }
    }
}

impl From<UuidShapeError> for EphemeralIdError {
    fn from(error: UuidShapeError) -> Self {
        match error {
            UuidShapeError::WrongLength { len } => Self::WrongLength { len },
            UuidShapeError::InvalidCharacter { position, value } => {
                Self::InvalidCharacter { position, value }
            }
        }
    }
}

/// Which of the two identity kinds a block target carries (ADR-0005).
///
/// Public block views expose this so a caller can tell durable identity from an ephemeral id
/// without matching on the selector itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockIdentityKind {
    /// The UUID stored in the block's Markdown.
    Durable,
    /// The random UUID the projection minted for a block that states no `id::`.
    Ephemeral,
}

/// A public block target: durable identity or a projection-scoped ephemeral id.
///
/// This is the only way a caller names one block. There is no positional variant: a position is a
/// coordinate inside one observation, never identity, and a batch of operations addresses positions
/// only against an explicit page version (ADR-0011).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockSelector {
    /// Durable identity: the `id::` UUID stored in Markdown.
    Durable(BlockId),
    /// Ephemeral identity: the random id the projection minted for this block.
    Ephemeral(EphemeralId),
}

impl BlockSelector {
    /// Which identity kind this selector carries.
    pub fn kind(&self) -> BlockIdentityKind {
        match self {
            Self::Durable(_) => BlockIdentityKind::Durable,
            Self::Ephemeral(_) => BlockIdentityKind::Ephemeral,
        }
    }

    /// The durable id, when this selector is the durable kind.
    pub fn durable_id(&self) -> Option<&BlockId> {
        match self {
            Self::Durable(id) => Some(id),
            Self::Ephemeral(_) => None,
        }
    }

    /// The ephemeral id, when this selector is the ephemeral kind.
    pub fn ephemeral_id(&self) -> Option<&EphemeralId> {
        match self {
            Self::Durable(_) => None,
            Self::Ephemeral(id) => Some(id),
        }
    }
}

impl fmt::Display for BlockSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Durable(id) => write!(formatter, "durable block {id}"),
            Self::Ephemeral(id) => write!(formatter, "ephemeral block {id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED_CASE: &str = "01234567-89aB-CdEf-0123-456789ABCDEF";
    const LOWERCASE: &str = "01234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn parse_accepts_mixed_case_and_lowercases() {
        let id = BlockId::parse(MIXED_CASE).expect("mixed-case UUID is valid");
        assert_eq!(id.as_str(), LOWERCASE);
        assert_eq!(id.to_string(), LOWERCASE);
        assert_eq!(format!("{id:?}"), format!("BlockId({LOWERCASE:?})"));
    }

    #[test]
    fn parse_rejects_wrong_length() {
        let error = BlockId::parse("012abcde-fghi-4567-89ab-cdef0123456").unwrap_err();
        assert_eq!(error, BlockIdError::WrongLength { len: 35 });
    }

    #[test]
    fn parse_rejects_misplaced_separator() {
        let mut value = LOWERCASE.to_owned();
        value.replace_range(9..10, "x");
        let error = BlockId::parse(&value).unwrap_err();
        assert_eq!(
            error,
            BlockIdError::InvalidCharacter {
                position: 9,
                value: 'x'
            }
        );
    }

    #[test]
    fn parse_rejects_non_hex_character() {
        let mut value = LOWERCASE.to_owned();
        value.replace_range(2..3, "g");
        let error = BlockId::parse(&value).unwrap_err();
        assert_eq!(
            error,
            BlockIdError::InvalidCharacter {
                position: 2,
                value: 'g'
            }
        );
    }

    #[test]
    fn ids_differing_only_in_case_are_equal() {
        let upper = BlockId::parse(&LOWERCASE.to_ascii_uppercase()).expect("valid UUID");
        let lower = BlockId::parse(LOWERCASE).expect("valid UUID");
        assert_eq!(upper, lower);
    }

    #[test]
    fn from_str_matches_parse() {
        assert_eq!(MIXED_CASE.parse::<BlockId>(), BlockId::parse(MIXED_CASE));
    }

    #[test]
    fn ephemeral_parse_accepts_mixed_case_and_lowercases() {
        let id = EphemeralId::parse(MIXED_CASE).expect("mixed-case UUID is valid");
        assert_eq!(id.as_str(), LOWERCASE);
        assert_eq!(id.to_string(), LOWERCASE);
        assert_eq!(format!("{id:?}"), format!("EphemeralId({LOWERCASE:?})"));
    }

    #[test]
    fn ephemeral_parse_rejects_the_same_shapes_the_durable_parse_does() {
        assert_eq!(
            EphemeralId::parse("012abcde-fghi-4567-89ab-cdef0123456").unwrap_err(),
            EphemeralIdError::WrongLength { len: 35 }
        );
        let mut value = LOWERCASE.to_owned();
        value.replace_range(2..3, "g");
        assert_eq!(
            EphemeralId::parse(&value).unwrap_err(),
            EphemeralIdError::InvalidCharacter {
                position: 2,
                value: 'g'
            }
        );
    }

    #[test]
    fn from_bytes_renders_the_canonical_shape() {
        let mut bytes = [0u8; 16];
        bytes[0] = 0x01;
        bytes[1] = 0x23;
        bytes[15] = 0xff;
        let id = EphemeralId::from_bytes(bytes);
        assert_eq!(id.as_str(), "01230000-0000-0000-0000-0000000000ff");
        // The rendered value parses back to itself, which is what the index stores and reads.
        assert_eq!(EphemeralId::parse(id.as_str()), Ok(id));
    }

    #[test]
    fn durable_selector_carries_durable_kind() {
        let id = BlockId::parse(LOWERCASE).expect("valid UUID");
        let selector = BlockSelector::Durable(id);
        assert_eq!(selector.kind(), BlockIdentityKind::Durable);
        assert_eq!(selector.durable_id().map(BlockId::as_str), Some(LOWERCASE));
        assert_eq!(selector.ephemeral_id(), None);
    }

    #[test]
    fn ephemeral_selector_carries_ephemeral_kind() {
        let id = EphemeralId::parse(LOWERCASE).expect("valid UUID");
        let selector = BlockSelector::Ephemeral(id);
        assert_eq!(selector.kind(), BlockIdentityKind::Ephemeral);
        assert_eq!(selector.durable_id(), None);
        assert!(selector.ephemeral_id().is_some());
    }
}
