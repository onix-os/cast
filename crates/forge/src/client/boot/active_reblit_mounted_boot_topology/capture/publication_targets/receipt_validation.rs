//! Stable installed-receipt validation for freshly retained boot targets.
//!
//! Receipt runtime witnesses describe a historical observation and are not
//! durable identity after reboot. This bridge therefore compares only the
//! exact destination shape, PARTUUID, and partition number against a fresh,
//! fully revalidated mounted topology before wrapping its opaque targets.

use thiserror::Error;

use crate::{
    boot_publication::{
        BootPublicationDestination, BootPublicationDestinations,
    },
    db::state::ExactPromotedBootPublicationReceiptChain,
};

use super::{
    ActiveReblitBootPublicationTargetsError,
    ReceiptValidatedActiveReblitBootPublicationTargets,
    RevalidatedActiveReblitMountedBootTopology,
};
use super::super::super::{
    BoundActiveReblitMountedBootTarget,
    BoundActiveReblitMountedBootTopology,
};

impl RevalidatedActiveReblitMountedBootTopology<'_> {
    /// Revalidate the live target attachments and bind them to the stable
    /// destination identities in one exact promoted installed receipt.
    pub(in crate::client) fn revalidate_promoted_receipt_targets<'view>(
        &'view self,
        chain: &ExactPromotedBootPublicationReceiptChain,
    ) -> Result<
        ReceiptValidatedActiveReblitBootPublicationTargets<'view>,
        ActiveReblitBootReceiptTargetValidationError,
    > {
        let receipt = chain.installed_receipt();
        let destinations = receipt.body().destinations();
        let stable = StableLiveBootDestinations::from_topology(self.topology());
        require_stable_destinations(stable, destinations)?;
        let targets = self
            .revalidate_publication_targets()
            .map_err(ActiveReblitBootReceiptTargetValidationError::Targets)?;
        require_stable_destinations(
            StableLiveBootDestinations::from_topology(self.topology()),
            destinations,
        )?;

        let aliases_esp = destinations.aliases_esp();
        if aliases_esp
            != matches!(
                &targets,
                super::RevalidatedActiveReblitBootPublicationTargets::BootAliasesEsp { .. }
            )
        {
            return Err(
                ActiveReblitBootReceiptTargetValidationError::TargetShapeMismatch,
            );
        }

        Ok(ReceiptValidatedActiveReblitBootPublicationTargets {
            targets,
            promoted_receipt: receipt.fingerprint(),
            aliases_esp,
        })
    }
}

#[derive(Clone, Copy)]
struct StableLiveBootDestination<'topology> {
    partuuid: &'topology str,
    partition_number: u32,
}

#[derive(Clone, Copy)]
enum StableLiveBootDestinations<'topology> {
    BootAliasesEsp {
        esp: StableLiveBootDestination<'topology>,
    },
    DistinctXbootldr {
        esp: StableLiveBootDestination<'topology>,
        xbootldr: StableLiveBootDestination<'topology>,
    },
}

impl<'topology> StableLiveBootDestinations<'topology> {
    fn from_topology(topology: BoundActiveReblitMountedBootTopology<'topology>) -> Self {
        match topology {
            BoundActiveReblitMountedBootTopology::BootAliasesEsp { esp } => {
                Self::BootAliasesEsp {
                    esp: StableLiveBootDestination::from_target(esp),
                }
            }
            BoundActiveReblitMountedBootTopology::DistinctXbootldr {
                esp,
                xbootldr,
            } => Self::DistinctXbootldr {
                esp: StableLiveBootDestination::from_target(esp),
                xbootldr: StableLiveBootDestination::from_target(xbootldr),
            },
        }
    }
}

impl<'topology> StableLiveBootDestination<'topology> {
    fn from_target(target: BoundActiveReblitMountedBootTarget<'topology>) -> Self {
        Self {
            partuuid: target.partuuid,
            partition_number: target.partition_number.get(),
        }
    }
}

fn require_stable_destinations(
    topology: StableLiveBootDestinations<'_>,
    destinations: &BootPublicationDestinations,
) -> Result<(), ActiveReblitBootReceiptTargetValidationError> {
    match (topology, destinations) {
        (
            StableLiveBootDestinations::BootAliasesEsp { esp },
            BootPublicationDestinations::BootAliasesEsp {
                esp: receipt_esp,
            },
        ) => require_stable_destination("esp", esp, receipt_esp),
        (
            StableLiveBootDestinations::DistinctXbootldr {
                esp,
                xbootldr,
            },
            BootPublicationDestinations::DistinctXbootldr {
                esp: receipt_esp,
                xbootldr: receipt_xbootldr,
            },
        ) => {
            require_stable_destination("esp", esp, receipt_esp)?;
            require_stable_destination("xbootldr", xbootldr, receipt_xbootldr)
        }
        _ => Err(ActiveReblitBootReceiptTargetValidationError::LayoutMismatch),
    }
}

fn require_stable_destination(
    destination: &'static str,
    live: StableLiveBootDestination<'_>,
    receipt: &BootPublicationDestination,
) -> Result<(), ActiveReblitBootReceiptTargetValidationError> {
    if live.partuuid != receipt.partuuid()
        || live.partition_number != receipt.partition_number()
    {
        Err(
            ActiveReblitBootReceiptTargetValidationError::StableIdentityMismatch {
                destination,
            },
        )
    } else {
        Ok(())
    }
}

/// Fail-closed stable target validation errors.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootReceiptTargetValidationError {
    #[error("the live boot topology and installed receipt have different destination layouts")]
    LayoutMismatch,
    #[error("the live {destination} PARTUUID or partition number differs from the installed receipt")]
    StableIdentityMismatch { destination: &'static str },
    #[error("the revalidated publication targets changed destination shape")]
    TargetShapeMismatch,
    #[error("revalidate live boot publication targets for the installed receipt")]
    Targets(#[source] ActiveReblitBootPublicationTargetsError),
}

#[cfg(test)]
#[path = "receipt_validation/tests.rs"]
mod tests;
