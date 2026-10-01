//! SeaLion distributed layer: coordinator, shards, replication, routing
//! (spec §60–71, milestones 13–15).
//!
//! - [`shard`]: document partitioning (`hash(doc_id) % N`), topology file,
//!   global BM25 statistics (§63).
//! - [`coordinator`]: parallel fan-out, local top-N under global stats,
//!   global merge, `partial` semantics (§62–65, §69).
//! - [`replica`]: health, round-robin/least-outstanding routing, verified
//!   segment copying (§66–68).
//! - [`rebalance`]: online shard movement (§71).
//!
//! Nodes are directories here (`<data>/nodes/<node>/`); placement,
//! file-copy, and routing mechanics are identical over a network, which is
//! explicitly the boundary of this milestone (no fake consensus).

pub mod coordinator;
pub mod rebalance;
pub mod replica;
pub mod shard;

pub use coordinator::{distributed_search, CoordinatorResult};
pub use shard::{shard_of, CopyHealth, Topology};
