//! Bounded raw-directory inventory foundation for a retained descriptor adapter.
//!
//! The source protocol is deliberately private and capability-free: callers
//! cannot provide a path, reopen closure, or mutation authority. The Linux
//! adapter consumes one already-owned, fresh directory description and feeds
//! each complete `getdents64` result into this parser without returning the
//! descriptor.

#[path = "production/budget.rs"]
mod budget;
#[path = "production/error.rs"]
mod error;
#[path = "production/inventory.rs"]
mod inventory;
#[path = "production/live.rs"]
mod live;
#[path = "production/model.rs"]
mod model;
#[path = "production/parser.rs"]
mod parser;
#[path = "production/retained.rs"]
mod retained;
#[path = "production/source.rs"]
mod source;

#[allow(unused_imports)] // named seam for the future retained descriptor observer
pub(crate) use error::ProductionRawDirectoryInventoryError;
#[allow(unused_imports)] // named seam for the future retained descriptor observer
pub(crate) use inventory::ProductionRawDirectoryInventory;
#[allow(unused_imports)] // named seam for the future retained descriptor observer
pub(crate) use model::ProductionRawDirectoryInventoryLimits;
#[allow(unused_imports)] // named seam for the future retained descriptor observer
pub(crate) use parser::parse_production_raw_directory_inventory_until;
#[allow(unused_imports)] // named seam for the future retained descriptor observer
pub(crate) use source::{ProductionRawDirectorySource, ProductionRawDirectorySourceError};

#[allow(unused_imports)] // aggregate accounting for the future retained observer
pub(crate) use model::ProductionRawDirectoryInventoryUsage;
#[allow(unused_imports)] // aggregate accounting for the future retained observer
pub(crate) use parser::parse_production_raw_directory_inventory_with_usage_until;

#[cfg(test)]
pub(crate) use retained::{
    FixtureFailedOpenDescriptorSlotUsage, FixtureRetainedBootNamespaceProtocolEvent,
    assess_retained_boot_namespace_with_hook_until, probe_failed_open_descriptor_slot_until,
};
pub(crate) use retained::{
    RetainedBootNamespaceAssessmentError, RetainedBootNamespaceAssessmentLimits, RetainedBootNamespaceExpectedSource,
    ValidatedRetainedBootNamespaceAssessment, assess_retained_boot_namespace_until,
};
pub(in crate::linux_fs) use retained::BoundRetainedBootFileSource;
