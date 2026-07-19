//! Descriptor-retained capture of one declared mounted boot topology.
//!
//! The prepared value owns the authenticated declarative intent, current-task
//! mount-context anchor, task-rooted attachment capabilities, and retained
//! sysfs partition identities.  It exposes only scalar facts after three
//! complete later observation passes.  Every operation shares one caller-owned
//! absolute deadline; no nested stage replaces it with a fresh timeout.
//!
//! This is consistency evidence, not permission to mutate either destination.
//! It proves no GPT role, filesystem type, physical disk, persistence,
//! durability, or publication authority and never opens a raw block device.

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
