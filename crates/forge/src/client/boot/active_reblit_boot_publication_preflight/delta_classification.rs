//! One-way bridge from retained preflight evidence to an inert boot delta.

use super::RevalidatedActiveReblitBootPublicationPreflight;
use crate::client::active_reblit_installed_boot_publication_delta::{
    ActiveReblitBootPublicationDeltaError,
    ClassifiedActiveReblitBootPublicationDelta,
    PreparedActiveReblitBootPublicationDelta,
};

impl<
        'plan,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
    RevalidatedActiveReblitBootPublicationPreflight<
        'plan,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
{
    /// Classify one authenticated installed-versus-desired request set using
    /// only this preflight's private, plan-bound live assessment.
    ///
    /// The result is inert data. It is not filesystem, receipt, journal, or
    /// cleanup authority, and every later mutation must reauthenticate the
    /// exact old and new byte identities retained by each entry.
    pub(in crate::client) fn classify_installed_boot_publication_delta(
        &self,
        prepared: &PreparedActiveReblitBootPublicationDelta,
    ) -> Result<ClassifiedActiveReblitBootPublicationDelta, ActiveReblitBootPublicationDeltaError>
    {
        prepared.classify_with_preflight_assessment(&self.assessment_seal)
    }
}
