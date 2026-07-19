//! Descriptor-retained capture of one declared mounted boot topology.
//!
//! The prepared value owns the authenticated declarative intent, current-task
//! mount-context anchor, task-rooted attachment capabilities, and retained
//! sysfs partition identities. It also retains a closed mountinfo policy
//! requiring `vfat`, per-mount `rw,nosuid,nodev,noexec,nosymfollow`, and
//! superblock `rw`. It exposes only scalar facts after three complete later
//! observation passes. Every operation shares one caller-owned absolute
//! deadline; no nested stage replaces it with a fresh timeout.
//!
//! This is consistency evidence, not permission to mutate either destination.
//! It proves no GPT role, descriptor-cross-checked filesystem identity,
//! physical disk, persistence, durability, or publication authority and never
//! opens a raw block device. Requiring `nosymfollow` gives future boot
//! publication an effective Linux 5.10-or-newer admission boundary without
//! changing the generic `linux_fs` Linux 5.6 compatibility baseline.

#[path = "capture/error.rs"]
mod error;
#[path = "capture/model.rs"]
mod model;
#[path = "capture/observation.rs"]
mod observation;
#[path = "capture/preparation.rs"]
mod preparation;

pub(in crate::client) use error::ActiveReblitMountedBootTopologyCaptureError;
pub(in crate::client) use model::{
    PreparedActiveReblitMountedBootTopology, RevalidatedActiveReblitMountedBootTopology,
};

#[cfg(test)]
pub(super) use error::ObservationBoundary;
#[cfg(test)]
pub(super) use model::FixtureMountInfoFeed;
#[cfg(test)]
pub(super) use observation::validate_fixture_attachment_selector;
