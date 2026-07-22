//! Pure cleanup classification for one exact promoted receipt chain.
//!
//! The durable loader authenticates the installed receipt and its named
//! predecessor. This module then validates the stable destination shape and
//! the alias-aware FAT path union before deriving inert cleanup decisions. It
//! deliberately ignores historical runtime witnesses: a later effect owner
//! must authenticate the live destinations again before using this data.

use std::{
    collections::{BTreeMap, TryReserveError, btree_map::Entry},
};

use thiserror::Error;

use crate::{
    boot_publication::{
        BootPublicationDestination, BootPublicationDestinations,
        BootPublicationOutput, BootPublicationOutputProvenanceClaim,
        BootPublicationReceiptFingerprint, BootPublicationRoot,
        MAX_BOOT_PUBLICATION_RECEIPT_OUTPUTS,
    },
    db::state::ExactPromotedBootPublicationReceiptChain,
};

const MAX_PROMOTED_BOOT_CLEANUP_UNION_OUTPUTS: usize =
    MAX_BOOT_PUBLICATION_RECEIPT_OUTPUTS * 2;

/// One inert cleanup classification derived from the exact receipt pair.
///
/// No variant grants filesystem, database, journal, descriptor, or mutation
/// authority. The enum is intentionally not cloneable.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitPromotedBootCleanupDisposition {
    NoOp,
    ReplaceOwned,
    DeleteOwnedStale,
    PreserveUnownedStale,
}

/// One predecessor output and its exact cleanup disposition.
///
/// A no-op or replacement also retains the exact installed output with the
/// same physical key. Stale classifications have no installed output. This
/// value is deliberately not cloneable.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) struct ActiveReblitPromotedBootCleanupPlanEntry<'chain> {
    disposition: ActiveReblitPromotedBootCleanupDisposition,
    predecessor_output: &'chain BootPublicationOutput,
    installed_output: Option<&'chain BootPublicationOutput>,
}

impl<'chain> ActiveReblitPromotedBootCleanupPlanEntry<'chain> {
    pub(in crate::client) const fn disposition(
        &self,
    ) -> &ActiveReblitPromotedBootCleanupDisposition {
        &self.disposition
    }

    pub(in crate::client) const fn predecessor_output(
        &self,
    ) -> &'chain BootPublicationOutput {
        self.predecessor_output
    }

    pub(in crate::client) const fn installed_output(
        &self,
    ) -> Option<&'chain BootPublicationOutput> {
        self.installed_output
    }
}

/// Bounded inert cleanup plan tied to one exact promoted receipt chain.
///
/// Keeping borrowed outputs prevents the plan from outliving or detaching
/// itself from the database-authenticated chain. The plan is intentionally
/// not cloneable and contains no effect authority.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) struct ActiveReblitPromotedBootCleanupPlan<'chain> {
    promoted_receipt: BootPublicationReceiptFingerprint,
    entries: Vec<ActiveReblitPromotedBootCleanupPlanEntry<'chain>>,
}

impl<'chain> ActiveReblitPromotedBootCleanupPlan<'chain> {
    pub(in crate::client) const fn promoted_receipt(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.promoted_receipt
    }

    pub(in crate::client) fn entries(
        &self,
    ) -> &[ActiveReblitPromotedBootCleanupPlanEntry<'chain>] {
        &self.entries
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PhysicalDestinationDomain {
    Shared,
    Esp,
    Boot,
}

struct IndexedOutput<'chain> {
    index: usize,
    output: &'chain BootPublicationOutput,
}

struct PhysicalOutputPair<'chain> {
    predecessor: Option<IndexedOutput<'chain>>,
    installed: Option<IndexedOutput<'chain>>,
}

type PhysicalOutputUnion<'chain> =
    BTreeMap<PhysicalDestinationDomain, BTreeMap<String, PhysicalOutputPair<'chain>>>;

impl ExactPromotedBootPublicationReceiptChain {
    /// Validate and classify cleanup work represented by this exact chain.
    ///
    /// Stable PARTUUID and partition-number identity must remain equal across
    /// the receipt pair. Historical runtime witnesses are intentionally not
    /// compared because device, inode, mount, and disk-sequence observations
    /// are not stable reboot identities.
    pub(in crate::client) fn prepare_active_reblit_promoted_boot_cleanup_plan(
        &self,
    ) -> Result<ActiveReblitPromotedBootCleanupPlan<'_>, ActiveReblitPromotedBootCleanupPlanError> {
        let installed = self.installed_receipt();
        let installed_outputs = installed.body().outputs();
        let predecessor = self.committed_predecessor_receipt();
        if let Some(predecessor) = predecessor {
            require_stable_destinations_match(
                predecessor.body().destinations(),
                installed.body().destinations(),
            )?;
        }

        let predecessor_outputs = predecessor.map_or(&[][..], |receipt| receipt.body().outputs());
        let union_capacity = installed_outputs
            .len()
            .checked_add(predecessor_outputs.len())
            .ok_or(ActiveReblitPromotedBootCleanupPlanError::UnionCountOverflow)?;
        if union_capacity > MAX_PROMOTED_BOOT_CLEANUP_UNION_OUTPUTS {
            return Err(ActiveReblitPromotedBootCleanupPlanError::UnionOutputLimit {
                actual: union_capacity,
            });
        }

        let mut predecessor_matches = Vec::new();
        predecessor_matches
            .try_reserve_exact(predecessor_outputs.len())
            .map_err(|source| ActiveReblitPromotedBootCleanupPlanError::Allocation {
                resource: "predecessor output matches",
                source,
            })?;
        predecessor_matches.resize_with(predecessor_outputs.len(), || None);

        let aliases_esp = installed.body().destinations().aliases_esp();
        let mut union = PhysicalOutputUnion::new();
        insert_installed_outputs(&mut union, installed_outputs, aliases_esp)?;
        insert_predecessor_outputs(
            &mut union,
            predecessor_outputs,
            aliases_esp,
            &mut predecessor_matches,
        )?;
        require_no_union_hierarchy_conflicts(&union)?;
        require_current_only_provenance(&union)?;

        let mut entries = Vec::new();
        entries
            .try_reserve_exact(predecessor_outputs.len())
            .map_err(|source| ActiveReblitPromotedBootCleanupPlanError::Allocation {
                resource: "cleanup plan entries",
                source,
            })?;
        for (predecessor_index, (predecessor_output, installed_index)) in predecessor_outputs
            .iter()
            .zip(predecessor_matches)
            .enumerate()
        {
            let installed_output = installed_index.map(|index| &installed_outputs[index]);
            let disposition = classify_predecessor_output(
                predecessor_index,
                predecessor_output,
                installed_index,
                installed_output,
            )?;
            entries.push(ActiveReblitPromotedBootCleanupPlanEntry {
                disposition,
                predecessor_output,
                installed_output,
            });
        }

        Ok(ActiveReblitPromotedBootCleanupPlan {
            promoted_receipt: installed.fingerprint(),
            entries,
        })
    }
}

fn require_stable_destinations_match(
    predecessor: &BootPublicationDestinations,
    installed: &BootPublicationDestinations,
) -> Result<(), ActiveReblitPromotedBootCleanupPlanError> {
    match (predecessor, installed) {
        (
            BootPublicationDestinations::BootAliasesEsp { esp: predecessor_esp },
            BootPublicationDestinations::BootAliasesEsp { esp: installed_esp },
        ) => require_stable_destination_match("esp", predecessor_esp, installed_esp),
        (
            BootPublicationDestinations::DistinctXbootldr {
                esp: predecessor_esp,
                xbootldr: predecessor_xbootldr,
            },
            BootPublicationDestinations::DistinctXbootldr {
                esp: installed_esp,
                xbootldr: installed_xbootldr,
            },
        ) => {
            require_stable_destination_match("esp", predecessor_esp, installed_esp)?;
            require_stable_destination_match(
                "xbootldr",
                predecessor_xbootldr,
                installed_xbootldr,
            )
        }
        _ => Err(ActiveReblitPromotedBootCleanupPlanError::DestinationLayoutMismatch),
    }
}

fn require_stable_destination_match(
    destination: &'static str,
    predecessor: &BootPublicationDestination,
    installed: &BootPublicationDestination,
) -> Result<(), ActiveReblitPromotedBootCleanupPlanError> {
    if predecessor.partuuid() != installed.partuuid()
        || predecessor.partition_number() != installed.partition_number()
    {
        return Err(ActiveReblitPromotedBootCleanupPlanError::StableDestinationMismatch {
            destination,
        });
    }
    Ok(())
}

fn insert_installed_outputs<'chain>(
    union: &mut PhysicalOutputUnion<'chain>,
    installed: &'chain [BootPublicationOutput],
    aliases_esp: bool,
) -> Result<(), ActiveReblitPromotedBootCleanupPlanError> {
    for (index, output) in installed.iter().enumerate() {
        let (domain, folded_path) = physical_key(aliases_esp, output)?;
        match union.entry(domain).or_default().entry(folded_path) {
            Entry::Vacant(entry) => {
                entry.insert(PhysicalOutputPair {
                    predecessor: None,
                    installed: Some(IndexedOutput { index, output }),
                });
            }
            Entry::Occupied(_) => {
                return Err(ActiveReblitPromotedBootCleanupPlanError::DuplicateInstalledPhysicalKey {
                    index,
                });
            }
        }
    }
    Ok(())
}

fn insert_predecessor_outputs<'chain>(
    union: &mut PhysicalOutputUnion<'chain>,
    predecessor: &'chain [BootPublicationOutput],
    aliases_esp: bool,
    predecessor_matches: &mut [Option<usize>],
) -> Result<(), ActiveReblitPromotedBootCleanupPlanError> {
    for (index, output) in predecessor.iter().enumerate() {
        let (domain, folded_path) = physical_key(aliases_esp, output)?;
        match union.entry(domain).or_default().entry(folded_path) {
            Entry::Vacant(entry) => {
                entry.insert(PhysicalOutputPair {
                    predecessor: Some(IndexedOutput { index, output }),
                    installed: None,
                });
            }
            Entry::Occupied(mut entry) => {
                let pair = entry.get_mut();
                if pair.predecessor.is_some() {
                    return Err(
                        ActiveReblitPromotedBootCleanupPlanError::DuplicatePredecessorPhysicalKey {
                            index,
                        },
                    );
                }
                let installed = pair
                    .installed
                    .as_ref()
                    .expect("an occupied predecessor-free union entry is installed");
                require_same_path_semantics(index, output, installed.index, installed.output)?;
                predecessor_matches[index] = Some(installed.index);
                pair.predecessor = Some(IndexedOutput { index, output });
            }
        }
    }
    Ok(())
}

fn require_same_path_semantics(
    predecessor_index: usize,
    predecessor: &BootPublicationOutput,
    installed_index: usize,
    installed: &BootPublicationOutput,
) -> Result<(), ActiveReblitPromotedBootCleanupPlanError> {
    if predecessor.root() != installed.root()
        || predecessor.relative_path() != installed.relative_path()
        || predecessor.phase() != installed.phase()
        || predecessor.role() != installed.role()
        || predecessor.mode() != installed.mode()
    {
        return Err(
            ActiveReblitPromotedBootCleanupPlanError::CrossReceiptPhysicalKeyMismatch {
                predecessor_index,
                installed_index,
            },
        );
    }
    Ok(())
}

fn require_no_union_hierarchy_conflicts(
    union: &PhysicalOutputUnion<'_>,
) -> Result<(), ActiveReblitPromotedBootCleanupPlanError> {
    for outputs in union.values() {
        for path in outputs.keys() {
            let mut ancestor = path.as_str();
            while let Some(separator) = ancestor.rfind('/') {
                ancestor = &ancestor[..separator];
                if outputs.contains_key(ancestor) {
                    return Err(
                        ActiveReblitPromotedBootCleanupPlanError::CrossReceiptHierarchyConflict,
                    );
                }
            }
        }
    }
    Ok(())
}

fn require_current_only_provenance(
    union: &PhysicalOutputUnion<'_>,
) -> Result<(), ActiveReblitPromotedBootCleanupPlanError> {
    for outputs in union.values() {
        for pair in outputs.values() {
            if pair.predecessor.is_some() {
                continue;
            }
            let installed = pair
                .installed
                .as_ref()
                .expect("a predecessor-free union entry is installed");
            if installed.output.provenance_claim()
                == BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast
            {
                return Err(
                    ActiveReblitPromotedBootCleanupPlanError::CurrentOnlyOwnershipClaim {
                        installed_index: installed.index,
                    },
                );
            }
        }
    }
    Ok(())
}

fn classify_predecessor_output(
    predecessor_index: usize,
    predecessor: &BootPublicationOutput,
    installed_index: Option<usize>,
    installed: Option<&BootPublicationOutput>,
) -> Result<ActiveReblitPromotedBootCleanupDisposition, ActiveReblitPromotedBootCleanupPlanError> {
    let predecessor_owned = claim_is_owned(predecessor.provenance_claim());
    let Some(installed) = installed else {
        return Ok(if predecessor_owned {
            ActiveReblitPromotedBootCleanupDisposition::DeleteOwnedStale
        } else {
            ActiveReblitPromotedBootCleanupDisposition::PreserveUnownedStale
        });
    };
    let installed_index = installed_index.expect("a matching installed output retains its index");
    let exact_bytes = predecessor.length() == installed.length()
        && predecessor.xxh3() == installed.xxh3()
        && predecessor.content_sha256() == installed.content_sha256();
    if exact_bytes {
        let expected_claim = if predecessor_owned {
            BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast
        } else {
            BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption
        };
        if installed.provenance_claim() != expected_claim {
            return Err(
                ActiveReblitPromotedBootCleanupPlanError::RetainedProvenanceMismatch {
                    predecessor_index,
                    installed_index,
                },
            );
        }
        return Ok(ActiveReblitPromotedBootCleanupDisposition::NoOp);
    }
    if !predecessor_owned {
        return Err(ActiveReblitPromotedBootCleanupPlanError::BorrowedReplacement {
            predecessor_index,
            installed_index,
        });
    }
    if installed.provenance_claim()
        != BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast
    {
        return Err(
            ActiveReblitPromotedBootCleanupPlanError::ReplacementProvenanceMismatch {
                predecessor_index,
                installed_index,
            },
        );
    }
    Ok(ActiveReblitPromotedBootCleanupDisposition::ReplaceOwned)
}

const fn claim_is_owned(claim: BootPublicationOutputProvenanceClaim) -> bool {
    matches!(
        claim,
        BootPublicationOutputProvenanceClaim::UnclaimedAbsent
            | BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast
    )
}

fn physical_key(
    aliases_esp: bool,
    output: &BootPublicationOutput,
) -> Result<(PhysicalDestinationDomain, String), ActiveReblitPromotedBootCleanupPlanError> {
    let domain = match (aliases_esp, output.root()) {
        (true, _) => PhysicalDestinationDomain::Shared,
        (false, BootPublicationRoot::Esp) => PhysicalDestinationDomain::Esp,
        (false, BootPublicationRoot::Boot) => PhysicalDestinationDomain::Boot,
    };
    let path = output.relative_path();
    let mut folded_path = String::new();
    folded_path.try_reserve_exact(path.len()).map_err(|source| {
        ActiveReblitPromotedBootCleanupPlanError::Allocation {
            resource: "FAT-folded receipt path",
            source,
        }
    })?;
    folded_path.extend(path.bytes().map(|byte| char::from(byte.to_ascii_lowercase())));
    Ok((domain, folded_path))
}

/// Closed validation failures while preparing the inert cleanup plan.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitPromotedBootCleanupPlanError {
    #[error("the predecessor and installed receipts have different destination layouts")]
    DestinationLayoutMismatch,
    #[error("the predecessor and installed receipts disagree on stable {destination} identity")]
    StableDestinationMismatch { destination: &'static str },
    #[error("the receipt-chain output count overflows")]
    UnionCountOverflow,
    #[error(
        "the receipt-chain union has {actual} outputs, exceeding limit {MAX_PROMOTED_BOOT_CLEANUP_UNION_OUTPUTS}"
    )]
    UnionOutputLimit { actual: usize },
    #[error("installed output {index} duplicates an alias-aware FAT physical key")]
    DuplicateInstalledPhysicalKey { index: usize },
    #[error("predecessor output {index} duplicates an alias-aware FAT physical key")]
    DuplicatePredecessorPhysicalKey { index: usize },
    #[error(
        "predecessor output {predecessor_index} and installed output {installed_index} share a physical key but disagree on path semantics"
    )]
    CrossReceiptPhysicalKeyMismatch {
        predecessor_index: usize,
        installed_index: usize,
    },
    #[error("the receipt-chain union has an ancestor/descendant physical-path conflict")]
    CrossReceiptHierarchyConflict,
    #[error("installed-only output {installed_index} falsely claims prior cast ownership")]
    CurrentOnlyOwnershipClaim { installed_index: usize },
    #[error(
        "retained predecessor output {predecessor_index} and installed output {installed_index} have an invalid provenance transition"
    )]
    RetainedProvenanceMismatch {
        predecessor_index: usize,
        installed_index: usize,
    },
    #[error(
        "borrowed predecessor output {predecessor_index} cannot be replaced by installed output {installed_index}"
    )]
    BorrowedReplacement {
        predecessor_index: usize,
        installed_index: usize,
    },
    #[error(
        "replacement predecessor output {predecessor_index} and installed output {installed_index} have an invalid provenance transition"
    )]
    ReplacementProvenanceMismatch {
        predecessor_index: usize,
        installed_index: usize,
    },
    #[error("allocate {resource}")]
    Allocation {
        resource: &'static str,
        #[source]
        source: TryReserveError,
    },
}

#[cfg(test)]
#[path = "active_reblit_promoted_boot_cleanup_plan_tests.rs"]
mod tests;
