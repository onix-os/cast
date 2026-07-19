//! Bounded raw-directory inventory foundation for a retained descriptor adapter.
//!
//! The source protocol is deliberately private and capability-free: callers
//! cannot provide a path, reopen closure, or mutation authority. A later Linux
//! adapter can issue `getdents64` against an already-retained directory and
//! feed each complete syscall result into this parser.

#[path = "production/budget.rs"]
mod budget;
#[path = "production/error.rs"]
mod error;
#[path = "production/inventory.rs"]
mod inventory;
#[path = "production/model.rs"]
mod model;
#[path = "production/parser.rs"]
mod parser;
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
