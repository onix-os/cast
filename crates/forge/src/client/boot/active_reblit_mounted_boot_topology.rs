//! Authenticated mounted ESP/XBOOTLDR topology evidence.
//!
//! The descriptor-retained capture layer owns authenticated declarative
//! intent, current-task mount context, task-rooted attachments, and sysfs
//! partition identities. It also retains a closed mountinfo policy requiring
//! `vfat`, per-mount `rw,nosuid,nodev,noexec,nosymfollow`, and superblock `rw`.
//! Every target additionally authenticates Linux MSDOS-family filesystem magic
//! against the exact retained destination descriptor identity; exact `vfat`
//! remains the separate mountinfo claim. It repeatedly observes those
//! capabilities and policy facts under one caller-owned deadline before
//! exposing the separate closed scalar model.
//! That scalar value owns no file, descriptor, path, raw mountinfo options,
//! sysfs bytes, or reopen authority and is the pure boundary for later
//! rendering.
//!
//! Success composes bounded mountinfo policy and descriptor-filesystem evidence
//! inside every topology pass. It does not prove a GPT partition role, physical
//! disk, persistence, ongoing mount currentness, durability, publication
//! authority, or permission to mutate ESP or XBOOTLDR. Requiring `nosymfollow`
//! gives future boot publication an effective Linux 5.10-or-newer admission
//! boundary without changing the generic `linux_fs` Linux 5.6 compatibility
//! baseline.

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

#[allow(unused_imports)] // consumed by the pure renderer and later durable publisher
pub(in crate::client) use capture::{
    ActiveReblitBootPublicationTargetsError, ActiveReblitMountedBootTopologyCaptureError,
    PreparedActiveReblitMountedBootTopology, RevalidatedActiveReblitBootPublicationTarget,
    RevalidatedActiveReblitBootPublicationTargets, RevalidatedActiveReblitMountedBootTopology,
};

#[cfg(test)]
#[path = "active_reblit_mounted_boot_topology_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "active_reblit_mounted_boot_topology_capture_tests.rs"]
mod capture_tests;

#[cfg(test)]
pub(in crate::client) use capture_tests::AliasFixture;

#[cfg(test)]
pub(in crate::client) use model::validated_boot_filesystem_evidence_fixture;
