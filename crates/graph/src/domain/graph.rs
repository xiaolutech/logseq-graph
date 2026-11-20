//! Identity and revision of a derived graph instance.

/// Which derived graph instance produced a result, and at which revision.
///
/// `instance_id` is opaque and changes whenever the derived state is rebuilt from scratch.
/// `revision` increases by one for every committed index write, so a caller can tell that two
/// results came from the same instance in a known order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphMetadata {
    instance_id: String,
    revision: u64,
}

impl GraphMetadata {
    pub(crate) fn new(instance_id: String, revision: u64) -> Self {
        Self {
            instance_id,
            revision,
        }
    }

    /// Opaque identifier of the derived graph instance.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Last committed index revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}
