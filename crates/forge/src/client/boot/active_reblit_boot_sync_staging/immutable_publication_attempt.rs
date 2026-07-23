//! Consuming entry from exact staged authority into immutable BOOT effects.

use crate::client::{
    Client,
    active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
    active_reblit_boot_publication_preflight::{
        ActiveReblitBootImmutablePublicationAttemptError,
        StagedExactActiveReblitBootPublication,
    },
};

use super::StagedActiveReblitBootSync;

impl<
        'plan,
        'inventory,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
    StagedActiveReblitBootSync<
        'plan,
        'inventory,
        BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
    >
where
    'input: 'plan,
{
    /// Consume this exact `BootSyncStarted` staging result into one immutable
    /// aggregate publication attempt.
    ///
    /// The plan is copied only as its already-retained reference. Initial
    /// durable admission occurs before topology preflight; the aggregate then
    /// repeats full staged validation immediately before its first effect.
    pub(in crate::client) fn attempt_immutable_boot_publication(
        self,
        client: &Client,
    ) -> Result<
        StagedExactActiveReblitBootPublication<
            'plan,
            'inventory,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootImmutablePublicationAttemptError,
    > {
        let plan = self.plan;
        {
            let fresh = self
                .revalidate_against(client)
                .map_err(ActiveReblitBootImmutablePublicationAttemptError::StagedAdmission)?;
            if !std::ptr::eq(fresh.plan(), plan) {
                return Err(
                    ActiveReblitBootImmutablePublicationAttemptError::StagedPlanMismatch {
                        checkpoint: "initial durable admission",
                    },
                );
            }
        }
        let preflight = plan
            .prepare_boot_publication_preflight()
            .map_err(ActiveReblitBootImmutablePublicationAttemptError::Preflight)?;
        preflight.publish_from_staged_authority(self, client)
    }
}
