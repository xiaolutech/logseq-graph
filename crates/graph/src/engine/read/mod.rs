use crate::engine::{Freshness, Graph};

mod block;
mod pages;
mod search;

pub(super) use block::{locate_block, BlockLookup};

/// Reads under one explicit freshness policy, without changing the graph's default.
///
/// Obtained from [`Graph::reads`]. Every method answers with the same [`crate::domain::Read`] shape as the
/// corresponding method on [`Graph`].
#[derive(Debug)]
pub struct Reads<'a> {
    graph: &'a mut Graph,
    freshness: Freshness,
}

impl Graph {
    /// Read under `freshness` instead of the policy these options installed.
    ///
    /// A graph opened with [`Freshness::CheckNow`] as its default scans once at open and again before
    /// every read, because the open scan is what makes the handle readable at all.
    pub fn reads(&mut self, freshness: Freshness) -> Reads<'_> {
        Reads {
            graph: self,
            freshness,
        }
    }
}
