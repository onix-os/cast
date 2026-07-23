//! Language-neutral declaration discovery and persistence contracts.

mod generated_slot;
mod fragment_set;
mod root_slot;
mod storage_error;

pub(crate) mod atomic_persistence;
pub(crate) mod managed_directory;

pub use generated_slot::GeneratedDeclarationSlot;
pub use fragment_set::{
    DiscoveredFragmentDeclaration, FragmentDeclarationLimits,
    FragmentDeclarationSet, FragmentDeclarationSetError,
};
pub use root_slot::{
    DiscoveredRootDeclaration, LanguageRegistrationError, RegisteredLanguages,
    RootDeclarationDiscoveryError, RootDeclarationSlot, RootDeclarationSlotError,
};
pub use storage_error::{
    DeleteDeclarationError, GeneratedDeclarationSlotError, SaveDeclarationError,
};
