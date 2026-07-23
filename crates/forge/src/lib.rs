pub use self::client::{
    Client, ClientBuilder, FrozenExecutableBinding, FrozenMaterialization, FrozenRootGuard, MaterializedFrozenRoot,
    ReadOnlyClient, ReadOnlyClientError,
};
pub use self::dependency::{Dependency, Provider};
pub use self::installation::Installation;
pub use self::package::Package;
pub use self::registry::Registry;
pub use self::repository::Repository;
pub use self::signal::Signal;
pub use self::state::State;
pub use self::system_model::SystemModel;

pub mod cli;
pub(crate) mod boot_publication;
pub mod client;
pub mod db;
pub mod dependency;
pub mod environment;
pub mod installation;
mod linux_fs;
pub mod package;
pub mod registry;
pub mod repository;
pub mod request;
pub mod runtime;
pub mod signal;
pub mod state;
pub mod system_model;
#[cfg(test)]
pub(crate) mod test_support;
// The journal codec and storage remain independently testable while the
// bounded identity guard begins consuming their exclusive lock. Journal
// creation and crash-reopen reconciliation are still separate work.
#[allow(dead_code)]
pub(crate) mod transition_journal;
// Durable tree identity keeps creation and recovery APIs isolated so the
// coordinator cannot accidentally mint or repair an identity during recovery.
pub(crate) mod transition_identity;
pub(crate) mod tree_marker;
pub mod util;
