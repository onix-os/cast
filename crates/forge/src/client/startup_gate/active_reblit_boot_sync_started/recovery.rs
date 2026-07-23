//! Resume receipt-owned boot cleanup from an exact promoted restart point.
//!
//! This coordinator retains the exact startup authority around every blocking
//! topology capture and every receipt-owned cleanup effect. Non-mutating
//! cleanup dispositions never reach the target adapter. Only after all owned
//! residue is reconciled does the coordinator consume the source authority
//! into the exact `BootSyncComplete` journal successor.

use crate::{
    client::{
        active_reblit_mounted_boot_topology::{
            ActiveReblitBootOwnedCleanupError,
            ActiveReblitMountedBootTopologyCaptureError,
            PreparedActiveReblitMountedBootTopology,
        },
        active_reblit_promoted_boot_cleanup_plan::ActiveReblitPromotedBootCleanupDisposition,
        startup_reconciliation::{
            ActiveReblitBootSyncStartedRecoveryAuthority,
            ActiveReblitBootSyncStartedRecoveryAuthorityError,
        },
        startup_recovery::{
            ActiveReblitBootSyncStartedCompletionPersistenceError,
            persist_active_reblit_boot_sync_started_completion_and_reopen,
        },
    },
    transition_journal::TransitionJournalStore,
};

use super::Dispatch;

pub(super) fn recover_promoted_cleanup_and_complete(
    journal: TransitionJournalStore,
    authority: ActiveReblitBootSyncStartedRecoveryAuthority<'_>,
) -> Result<Dispatch, Error> {
    authority.revalidate(&journal)?;
    let preparation =
        PreparedActiveReblitMountedBootTopology::prepare(authority.installation());
    let post_preparation_authority = authority.revalidate(&journal);
    let prepared = match (preparation, post_preparation_authority) {
        (Ok(prepared), Ok(())) => prepared,
        (Err(source), Ok(())) => return Err(Error::PrepareTopology(source)),
        (Ok(_), Err(source)) => {
            return Err(Error::PostPreparationAuthority(source));
        }
        (Err(topology), Err(authority)) => {
            return Err(Error::PreparationAndAuthority {
                topology,
                authority,
            });
        }
    };

    authority.revalidate(&journal)?;
    let topology_revalidation = prepared.revalidate(authority.installation());
    let post_topology_authority = authority.revalidate(&journal);
    let topology = match (topology_revalidation, post_topology_authority) {
        (Ok(topology), Ok(())) => topology,
        (Err(source), Ok(())) => return Err(Error::RevalidateTopology(source)),
        (Ok(_), Err(source)) => {
            return Err(Error::PostTopologyAuthority(source));
        }
        (Err(topology), Err(authority)) => {
            return Err(Error::TopologyAndAuthority {
                topology,
                authority,
            });
        }
    };

    let targets = authority
        .revalidate_promoted_receipt_targets(&journal, &topology)?;
    let cleanup_plan = authority.cleanup_plan(&journal)?;
    for (entry_index, entry) in cleanup_plan.entries().iter().enumerate() {
        match entry.disposition() {
            ActiveReblitPromotedBootCleanupDisposition::NoOp
            | ActiveReblitPromotedBootCleanupDisposition::PreserveUnownedStale => {}
            ActiveReblitPromotedBootCleanupDisposition::ReplaceOwned
            | ActiveReblitPromotedBootCleanupDisposition::DeleteOwnedStale => {
                authority.revalidate(&journal)?;
                let cleanup = targets.reconcile_and_cleanup_restart_receipt_entry(
                    &cleanup_plan,
                    entry_index,
                    authority.cleanup_seal(),
                );
                let post_cleanup_authority = authority.revalidate(&journal);
                match (cleanup, post_cleanup_authority) {
                    (Ok(_), Ok(())) => {}
                    (Err(source), Ok(())) => return Err(Error::Cleanup(source)),
                    (Ok(_), Err(source)) => {
                        return Err(Error::PostCleanupAuthority(source));
                    }
                    (Err(cleanup), Err(authority)) => {
                        return Err(Error::CleanupAndPostCleanupAuthority {
                            cleanup,
                            authority,
                        });
                    }
                }
            }
        }
    }
    authority.revalidate(&journal)?;

    drop(cleanup_plan);
    drop(targets);
    drop(topology);
    drop(prepared);

    let (journal, record) =
        persist_active_reblit_boot_sync_started_completion_and_reopen(
            journal, authority,
        )?;
    Ok(Dispatch::Handled { journal, record })
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum Error {
    #[error("revalidate exact promoted ActiveReblit BootSyncStarted recovery authority")]
    Authority(#[from] ActiveReblitBootSyncStartedRecoveryAuthorityError),
    #[error("prepare mounted boot topology for promoted restart cleanup")]
    PrepareTopology(#[source] ActiveReblitMountedBootTopologyCaptureError),
    #[error("revalidate restart authority after mounted boot-topology preparation")]
    PostPreparationAuthority(
        #[source] ActiveReblitBootSyncStartedRecoveryAuthorityError,
    ),
    #[error("mounted boot-topology preparation and its closing authority revalidation both failed: {authority}")]
    PreparationAndAuthority {
        #[source]
        topology: ActiveReblitMountedBootTopologyCaptureError,
        authority: ActiveReblitBootSyncStartedRecoveryAuthorityError,
    },
    #[error("revalidate mounted boot topology for promoted restart cleanup")]
    RevalidateTopology(#[source] ActiveReblitMountedBootTopologyCaptureError),
    #[error("revalidate restart authority after mounted boot-topology revalidation")]
    PostTopologyAuthority(
        #[source] ActiveReblitBootSyncStartedRecoveryAuthorityError,
    ),
    #[error("mounted boot-topology revalidation and its closing authority revalidation both failed: {authority}")]
    TopologyAndAuthority {
        #[source]
        topology: ActiveReblitMountedBootTopologyCaptureError,
        authority: ActiveReblitBootSyncStartedRecoveryAuthorityError,
    },
    #[error("reconcile one receipt-owned promoted restart cleanup entry")]
    Cleanup(#[from] ActiveReblitBootOwnedCleanupError),
    #[error("revalidate restart authority after receipt-owned cleanup")]
    PostCleanupAuthority(
        #[source] ActiveReblitBootSyncStartedRecoveryAuthorityError,
    ),
    #[error("receipt-owned cleanup and its closing authority revalidation both failed: {authority}")]
    CleanupAndPostCleanupAuthority {
        #[source]
        cleanup: ActiveReblitBootOwnedCleanupError,
        authority: ActiveReblitBootSyncStartedRecoveryAuthorityError,
    },
    #[error("persist exact promoted restart cleanup as BootSyncComplete")]
    Persistence(
        #[from] ActiveReblitBootSyncStartedCompletionPersistenceError,
    ),
}
