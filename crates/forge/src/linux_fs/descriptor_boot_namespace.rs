//! Pure descriptor-boot destination namespace assessment.
//!
//! This foundation classifies canonical requested relative destinations as
//! absent, byte-exact, or stably different. Namespace and content observations
//! are injected through a private protocol: this slice contains no production
//! filesystem calls and accepts no path, file, descriptor, reopen closure, or
//! mutation authority. A later descriptor adapter can implement the same
//! closed observation schedule without changing the scalar result model.

#[path = "descriptor_boot_namespace/budget.rs"]
mod budget;
#[path = "descriptor_boot_namespace/classifier.rs"]
mod classifier;
#[path = "descriptor_boot_namespace/error.rs"]
mod error;
#[cfg(test)]
#[path = "descriptor_boot_namespace/fixture.rs"]
mod fixture;
#[path = "descriptor_boot_namespace/model.rs"]
mod model;
#[path = "descriptor_boot_namespace/observer.rs"]
mod observer;
#[allow(dead_code)] // retained-descriptor integration follows this parser foundation
#[path = "descriptor_boot_namespace/production.rs"]
mod production;
#[path = "descriptor_boot_namespace/trie.rs"]
mod trie;

#[allow(unused_imports)] // named surface for the later retained-descriptor adapter
pub(crate) use error::BootNamespaceAssessmentError;
#[allow(unused_imports)] // named surface for the later retained-descriptor adapter
pub(crate) use model::{
    BootNamespaceAssessmentLimits, BootNamespaceDestinationState, BootNamespaceRequest,
    ValidatedBootNamespaceAssessment,
};

#[cfg(test)]
pub(crate) use classifier::assess_fixture_boot_namespace_until;
#[cfg(test)]
pub(crate) use fixture::{
    FixtureBootNamespace, FixtureBootNamespaceProtocolEvent, FixtureDirectory, FixtureDirectoryEntry,
    FixtureExpectedStream, FixtureLookup, FixtureRegularFile,
};
#[cfg(test)]
pub(crate) use model::FixtureBootNamespaceUsage;
#[cfg(test)]
pub(crate) use observer::{
    BootNamespaceLookup, BootNamespaceNodeIdentity, BootNamespaceNodeKind, BootNamespaceObservationBoundary,
    BootNamespaceRegularWitness,
};
#[cfg(test)]
pub(crate) use production::{
    ProductionRawDirectoryInventory, ProductionRawDirectoryInventoryError, ProductionRawDirectoryInventoryLimits,
    ProductionRawDirectoryInventoryUsage, ProductionRawDirectorySource, ProductionRawDirectorySourceError,
    parse_production_raw_directory_inventory_until, parse_production_raw_directory_inventory_with_usage_until,
};
