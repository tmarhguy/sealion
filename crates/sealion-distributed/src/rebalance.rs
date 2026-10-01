//! Online shard movement (spec §71): copy → verify → flip routing →
//! retire source, with queries served throughout.
//!
//! The move copies immutable segment files to the destination node,
//! verifies every copy by opening it (checksums), registers the new copy
//! in the topology with one atomic store, and only then deletes the
//! source files. A crash before the store leaves the old routing live; a
//! crash after the store leaves an unreferenced source directory (orphan,
//! harmless, cleaned by the next move or GC).

use std::path::Path;

use crate::replica::copy_segments_verified;
use crate::shard::{shard_copy_dir, CopyHealth, ShardCopy, Topology};

/// Move shard `shard` from `src_node` to `dst_node`. `dst_node` is created
/// if unknown. The source copy is retired (files removed, topology entry
/// dropped) only after the destination verifies.
pub fn move_shard(
    data_dir: &Path,
    topo: &mut Topology,
    shard: usize,
    src_node: &str,
    dst_node: &str,
) -> Result<Vec<String>, String> {
    if src_node == dst_node {
        return Err("source and destination nodes are identical".into());
    }
    if !topo.nodes.iter().any(|n| n == dst_node) {
        topo.nodes.push(dst_node.to_string());
    }
    let src = shard_copy_dir(data_dir, src_node, shard);
    let dst = shard_copy_dir(data_dir, dst_node, shard);
    if !src.exists() {
        return Err(format!("source copy missing: {}", src.display()));
    }
    // 1–2. Copy + verify (throws before anything is visible).
    let copied = copy_segments_verified(&src, &dst)?;
    // 3. Flip routing atomically: register destination, drop source.
    let generation = topo
        .copies
        .iter()
        .filter(|c| c.shard == shard)
        .map(|c| c.generation)
        .max()
        .unwrap_or(0)
        + 1;
    topo.copies
        .retain(|c| !(c.shard == shard && c.node == src_node));
    // Retire-then-add would briefly strand the shard; add-then-store keeps
    // both live in one atomic publish (source files still on disk).
    topo.copies.push(ShardCopy {
        shard,
        node: dst_node.to_string(),
        generation,
        health: CopyHealth::Healthy,
        last_heartbeat_ms: 0,
    });
    topo.store(data_dir)?;
    // 4. Retire source files (best effort after the flip).
    let _ = std::fs::remove_dir_all(&src);
    Ok(copied)
}
