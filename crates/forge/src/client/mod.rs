//! The core client implementation for Cast's package manager
//!
//! A [`Client`] needs to be constructed to handle the initialisation of various
//! databases, plugins and data sources to centralise package query and management
//! operations

use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString, OsStr, OsString},
    fmt,
    io::{self, Read},
    mem::MaybeUninit,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::MetadataExt,
        },
    },
    path::{Component as PathComponent, Path, PathBuf},
    ptr::NonNull,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::os::unix::fs::PermissionsExt as _;

use astr::AStr;
use filetime::FileTime;
use fs_err as fs;
use futures_util::{StreamExt, TryStreamExt, stream};
use itertools::Itertools;
use nix::{
    errno::Errno,
    fcntl::{self, OFlag},
    libc::{AT_FDCWD, RENAME_NOREPLACE, SYS_renameat2, syscall},
    sys::stat::{Mode, fchmod, fchmodat, mkdirat},
    unistd::{UnlinkatFlags, linkat, read, symlinkat, unlinkat, write},
};
use postblit::TriggerScope;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use stone::{StoneDecodedPayload, StoneDigestWriterHasher, StonePayloadLayoutFile, StonePayloadLayoutRecord};
use thiserror::Error;
use tracing::{info, info_span, trace};
use tui::{MultiProgress, ProgressBar, ProgressStyle, Styled};
use vfs::tree::{BlitFile, Element, builder::TreeBuilder};

use self::external_materialization::{ExternalMaterializationAdmission, RetainedExternalMaterializationTarget};
use self::install::install;
use self::prune::{prune_cache, prune_states};
use self::remove::remove;
use self::sync::sync;
use self::verify::verify;
use crate::{
    Installation, Package, Provider, Registry, Signal, State, SystemModel,
    client::fetch::fetch,
    db, environment, installation,
    linux_fs::{
        chmod_path_descriptor, chmod_path_descriptor_until, open_path_descriptor_readonly_until, openat2_file,
        openat2_file_until, renameat2_noreplace_until, require_no_access_acl_until, require_no_default_acl,
        require_no_default_acl_until, set_path_descriptor_times_until, sync_filesystem_until,
    },
    package,
    registry::plugin::{self, Plugin},
    repository, runtime, signal,
    state::{self, Selection},
    system_model::{self, LoadedSystemModel},
    transition_identity::{
        ArchivedCandidateError, FailedCandidateKind, QuarantinedCandidate, RetainedArchivedCandidateMoveFailure,
        RetainedArchivedCandidateMoveOutcome, RetainedExchangeFailure, RetainedExchangeOutcome,
        RetainedPreviousMoveFailure, RetainedPreviousMoveOutcome, RetainedStagingWrapperRotationFailure,
        RetainedStagingWrapperRotationOutcome, StatefulTreeIdentity,
    },
};

pub use self::extract::extract;
pub use self::index::index;
pub use self::read_only::{ReadOnlyClient, ReadOnlyClientError};
pub use self::resolve::{AvailableClosure, Error as ResolveError, ResolvedPackage, ResolvedRequest};
pub use self::self_upgrade::self_upgrade;

#[allow(dead_code)] // DB-only substrate; consumed by the later asset-freeze slice
#[path = "boot/active_reblit_projection.rs"]
pub(crate) mod active_reblit_boot_projection;
#[cfg(test)]
mod active_reblit_tests;
mod active_state_authority;
#[cfg(test)]
mod active_state_authority_tests;
mod active_state_snapshot;
#[cfg(test)]
mod active_state_snapshot_tests;
mod archived_repair;
mod archived_repair_materialization;
#[cfg(test)]
mod archived_repair_tests;
mod boot;
#[allow(dead_code)] // pre-claim substrate; wired only after the worker input model is complete
#[path = "boot/asset_snapshots.rs"]
mod boot_asset_snapshots;
mod cache;
mod candidate_metadata;
mod clean_boot_synchronization;
mod external_materialization;
mod fetch;
mod fixed_staging;
mod install;
mod journal_usr_exchange_authority;
mod legacy_boot_repair;
#[cfg(test)]
mod mutable_startup_namespace_tests;
mod mutable_system_capabilities;
mod postblit;
mod read_only;
mod remove;
mod resolve;
mod self_upgrade;
mod startup_gate;
#[cfg(test)]
mod startup_gate_tests;
mod startup_reconciliation;
mod startup_recovery;
#[cfg(test)]
#[path = "startup_recovery/forward_origin_test_support.rs"]
mod startup_recovery_forward_origin_test_support;
use mutable_system_capabilities::{MutableSystemCapabilities, open_mutable_system_capabilities};
#[cfg(test)]
pub(in crate::client) use mutable_system_capabilities::{
    MutableSystemCapabilitiesTestSeal, arm_after_system_database_open,
};
pub(crate) use startup_reconciliation::ActiveReblitReplacementMutationAuthorityProvider;
#[cfg(test)]
pub(crate) use startup_recovery_forward_origin_test_support::{
    assert_reverse_exchange_intent_recovers_to_usr_restored,
    assert_usr_exchange_intent_post_recovers_to_pending_reverse,
    assert_usr_restored_routes_to_candidate_preserve_intent,
    assert_usr_rollback_decision_routes_to_reverse_exchange_intent, snapshot_startup_recovery_namespace,
};
mod sync;
mod transaction_root;
mod verify;

#[allow(unused_imports)] // contract-only until the journal coordinator is live-wired
pub(crate) use journal_usr_exchange_authority::{
    AppliedJournalUsrExchangeAuthority, JournalUsrExchangeAuthority, JournalUsrExchangeAuthorityError,
    JournalUsrExchangeAuthorityPreflight, JournalUsrExchangePreparationSeal,
};

pub mod extract;
pub mod index;
pub mod prune;

include!("core/construction.rs");
include!("core/client_model.rs");
include!("core/client_facade.rs");
include!("core/state_planning.rs");
include!("core/stateful_transition.rs");
include!("core/stateful_recovery.rs");
include!("core/ephemeral_transition.rs");
include!("core/package_cache_orchestration.rs");
include!("core/materialization_facade.rs");
include!("core/state_queries.rs");
include!("frozen/model.rs");
include!("frozen/layout_resolution.rs");
include!("frozen/executable_format.rs");
include!("frozen/executable_identity.rs");
include!("frozen/root_anchor.rs");
include!("core/root_abi.rs");
include!("frozen/private_stage.rs");
include!("frozen/publication.rs");
include!("frozen/discard.rs");
include!("frozen/normalization_execution.rs");
include!("frozen/normalization_verification.rs");
include!("materialization/layout_planning.rs");
include!("materialization/tree_blit.rs");
include!("materialization/assets.rs");
include!("core/state_metadata.rs");
include!("core/operation_scope.rs");
include!("materialization/pending_file.rs");
include!("core/registry_build.rs");
include!("materialization/blit_stats.rs");
include!("core/error.rs");
include!("core/error_conversions.rs");

#[cfg(test)]
mod tests;
