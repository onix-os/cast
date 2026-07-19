use std::collections::TryReserveError;

use thiserror::Error;

use super::{
    super::{
        active_reblit_boot_inputs::ActiveReblitStoneBootInputsError,
        active_reblit_boot_schema_inputs::ActiveReblitBootSchemaInputsError,
        active_reblit_local_boot_policy::ActiveReblitLocalBootPolicyError,
        active_reblit_package_cmdline_inputs::ActiveReblitPackageCmdlineInputsError,
        active_reblit_root_filesystem_intent::ActiveReblitRootFilesystemIntentError,
    },
    state,
};
use crate::transition_identity::ActiveReblitBootStateRootsError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitCmdlineSource {
    Package { binding_index: u16 },
    LocalAppend { entry_index: u16 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitCmdlineTokenReason {
    NonPrintableAscii,
    UnsupportedQuoteOrEscape,
    EndOfOptionsSeparator,
    EmptyKey,
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootRenderInputsError {
    #[error("boot render-input deadline expired at {checkpoint}")]
    DeadlineExceeded { checkpoint: &'static str },
    #[error("revalidate the exact Stone boot-input owner")]
    Stone(#[from] ActiveReblitStoneBootInputsError),
    #[error("revalidate the exact state-root owner")]
    StateRoots(#[from] ActiveReblitBootStateRootsError),
    #[error("prepare authenticated package command-line inputs")]
    PackageCmdlines(#[from] ActiveReblitPackageCmdlineInputsError),
    #[error("prepare authenticated boot schemas")]
    Schemas(#[from] ActiveReblitBootSchemaInputsError),
    #[error("revalidate authenticated machine-local boot policy")]
    LocalPolicy(#[from] ActiveReblitLocalBootPolicyError),
    #[error("revalidate authenticated root-filesystem intent")]
    RootIntent(#[from] ActiveReblitRootFilesystemIntentError),
    #[error("package command-line projection no longer equals the exact Stone projection")]
    PackageProjectionChanged,
    #[error("boot render-input kernel count {actual} exceeds limit {limit}")]
    KernelCountLimit { limit: usize, actual: usize },
    #[error("authenticated Stone inputs and eligible state roots have no renderable kernel")]
    NoRenderableKernel,
    #[error("Stone binding index {actual} exceeds limit {limit}")]
    BindingIndexLimit { limit: usize, actual: usize },
    #[error("prepared Stone owner does not contain exactly one systemd-boot coordinate (got {actual})")]
    SystemdBootCoordinateCount { actual: usize },
    #[error("Stone systemd-boot coordinate {binding_index} is no longer exact")]
    SystemdBootCoordinateChanged { binding_index: u16 },
    #[error("duplicate kernel coordinate for state {state}, version {version}")]
    DuplicateKernel { state: i32, version: Box<str> },
    #[error("Stone kernel coordinate {binding_index} is no longer exact")]
    KernelCoordinateChanged { binding_index: u16 },
    #[error("Stone initrd coordinate {binding_index} is no longer exact")]
    InitrdCoordinateChanged { binding_index: u16 },
    #[error("command-line token from {origin:?} is invalid: {reason:?}")]
    InvalidCmdlineToken {
        origin: ActiveReblitCmdlineSource,
        reason: ActiveReblitCmdlineTokenReason,
    },
    #[error("command-line token from {origin:?} authors reserved key {key}")]
    ReservedCmdlineKey {
        origin: ActiveReblitCmdlineSource,
        key: &'static str,
    },
    #[error("root-filesystem authority returned an inexact kernel argument")]
    InvalidRootArgument,
    #[error("kernel command line for state {state}, version {version} has {actual} tokens, limit {limit}")]
    KernelCmdlineTokenLimit {
        state: i32,
        version: Box<str>,
        limit: usize,
        actual: usize,
    },
    #[error("kernel command line for state {state}, version {version} has {actual} bytes, limit {limit}")]
    KernelCmdlineByteLimit {
        state: i32,
        version: Box<str>,
        limit: usize,
        actual: usize,
    },
    #[error("aggregate kernel command lines have {actual} tokens, limit {limit}")]
    AggregateCmdlineTokenLimit { limit: usize, actual: usize },
    #[error("aggregate kernel command lines have {actual} bytes, limit {limit}")]
    AggregateCmdlineByteLimit { limit: usize, actual: usize },
    #[error("state {state} cannot be formatted as one canonical cast.fstx token")]
    InvalidStateToken { state: i32 },
    #[error("allocate {resource} while preparing boot render inputs")]
    Allocation {
        resource: &'static str,
        #[source]
        source: TryReserveError,
    },
}

pub(super) fn duplicate_kernel(state_id: state::Id, version: &str) -> ActiveReblitBootRenderInputsError {
    ActiveReblitBootRenderInputsError::DuplicateKernel {
        state: i32::from(state_id),
        version: version.to_owned().into_boxed_str(),
    }
}
