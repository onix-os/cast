//! Language-neutral declaration discovery and persistence contracts.

mod generated_slot;
mod evaluator_set;
mod fixed_root_load_error;
mod fixed_root_loader;
mod fragment_set;
mod loaded;
mod manager;
mod manager_error;
mod rooted_load_error;
mod rooted_loader;
mod rooted_fragment_set;
mod root_slot;
mod storage_error;

pub(crate) mod atomic_persistence;
pub(crate) mod managed_directory;

pub use generated_slot::GeneratedDeclarationSlot;
pub use evaluator_set::{
    ConfigDeclarationEvaluator, DeclarationEvaluatorSet,
    DeclarationEvaluatorSetError, TypedDeclarationEvaluatorSet,
};
pub use fixed_root_load_error::{
    FixedRootAuthorityError, FixedRootRevalidationPhase,
    LoadFixedRootDeclarationError,
};
pub use fixed_root_loader::load_fixed_root_declaration;
pub use fragment_set::{
    DiscoveredFragmentDeclaration, FragmentDeclarationLimits,
    FragmentDeclarationSet, FragmentDeclarationSetError,
};
pub use loaded::LoadedDeclaration;
pub use manager_error::{
    DeleteManagedDeclarationError, DeclarationRevalidationPhase,
    LoadManagedDeclarationError, SaveManagedDeclarationError,
};
pub use rooted_load_error::{
    LoadRootedDeclarationsError, RootedDeclarationRevalidationPhase,
};
pub use rooted_loader::load_rooted_declarations;
pub use rooted_fragment_set::{
    RootedFragmentDeclaration, RootedFragmentDeclarationSet,
    RootedFragmentDeclarationSetError,
};
pub use root_slot::{
    DiscoveredRootDeclaration, LanguageRegistrationError, RegisteredLanguages,
    RootDeclarationDiscoveryError, RootDeclarationSlot, RootDeclarationSlotError,
};
pub use storage_error::{
    DeleteDeclarationError, GeneratedDeclarationSlotError, SaveDeclarationError,
};
