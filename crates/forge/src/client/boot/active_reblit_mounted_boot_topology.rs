//! Authenticated mounted ESP/XBOOTLDR topology evidence.
//!
//! The descriptor-retained capture layer owns authenticated declarative
//! intent, current-task mount context, task-rooted attachments, and sysfs
//! partition identities.  It repeatedly observes those capabilities under one
//! caller-owned deadline before exposing the separate closed scalar model.
//! That scalar value owns no file, descriptor, path, mountinfo snapshot, sysfs
//! bytes, or reopen authority and is the pure boundary for later rendering.
//!
//! Success records bounded consistency evidence only. It does not prove a GPT
//! partition role, filesystem type, physical disk, persistence, ongoing mount
//! currentness, durability, publication authority, or permission to mutate
//! ESP or XBOOTLDR.

#[path = "active_reblit_mounted_boot_topology/capture.rs"]
mod capture;
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

#[allow(unused_imports)] // consumed by the later boot renderer/publisher slice
pub(in crate::client) use capture::{
    ActiveReblitMountedBootTopologyCaptureError, PreparedActiveReblitMountedBootTopology,
    RevalidatedActiveReblitMountedBootTopology,
};

#[cfg(test)]
#[path = "active_reblit_mounted_boot_topology_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "active_reblit_mounted_boot_topology_capture_tests.rs"]
mod capture_tests;
