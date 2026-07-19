//! Closed scalar facts for authenticated mounted ESP/XBOOTLDR topology.
//!
//! This module is the pure boundary between future descriptor-retained
//! observation and later boot rendering. It owns only exact declarative
//! selectors and scalar identity facts. It owns no file, descriptor, path,
//! mountinfo snapshot, sysfs bytes, or reopen authority.
//!
//! A successful value records consistency evidence only. It does not prove a
//! GPT partition role, filesystem type, physical disk, persistence, ongoing
//! mount currentness, durability, publication authority, or permission to
//! mutate ESP or XBOOTLDR. The future coordinator must establish every input
//! fact under one retained namespace epoch and one caller-owned deadline.

#[path = "active_reblit_mounted_boot_topology/error.rs"]
mod error;
#[path = "active_reblit_mounted_boot_topology/model.rs"]
mod model;
#[path = "active_reblit_mounted_boot_topology/validation.rs"]
mod validation;

#[allow(unused_imports)] // consumed by the descriptor-retained coordinator slice
pub(in crate::client) use error::ActiveReblitMountedBootTopologyError;
#[allow(unused_imports)] // consumed by the descriptor-retained coordinator slice
pub(in crate::client) use model::{
    ActiveReblitMountedBootTopology, ActiveReblitMountedBootTopologyObservation, BootTargetRole,
    BoundActiveReblitMountedBootTarget, BoundActiveReblitMountedBootTopology, MountedBootDestinationIdentity,
    MountedBootTargetObservation, ObservationPhase,
};

#[cfg(test)]
#[path = "active_reblit_mounted_boot_topology_tests.rs"]
mod tests;
