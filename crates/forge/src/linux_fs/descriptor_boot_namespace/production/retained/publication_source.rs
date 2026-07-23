//! Opaque single-source bridge for descriptor-retained boot-file publication.
//!
//! The bridge stays inside `linux_fs`: it exposes neither the sealed source
//! descriptor nor a reopen operation. Binding authenticates the same generated
//! or fully sealed source protocol used by destination assessment, and every
//! later read remains positional and budgeted under the original deadline.

use std::time::Instant;

use super::super::super::model::BootNamespaceRequest;
use super::{
    error::RetainedBootNamespaceAssessmentError,
    expected::{
        BoundExpectedSourceEvidence, RetainedBootNamespaceExpectedSource, bind_expected_streams, read_expected,
        terminally_revalidate_expected_streams,
    },
    limits::{LiveLedger, RetainedBootNamespaceAssessmentLimits},
};

/// One exact expected source bound for a single immutable publication leaf.
///
/// This type is visible only within `linux_fs`. It owns no source descriptor,
/// cannot be cloned, and can release bytes only through bounded positional
/// reads tied to the exact request that authenticated it.
pub(in crate::linux_fs) struct BoundRetainedBootFileSource<'request, 'expected, 'source> {
    request: BootNamespaceRequest<'request>,
    source: &'expected RetainedBootNamespaceExpectedSource<'source>,
    evidence: BoundExpectedSourceEvidence,
    ledger: LiveLedger,
}

impl<'request, 'expected, 'source> BoundRetainedBootFileSource<'request, 'expected, 'source> {
    pub(in crate::linux_fs) fn bind_until(
        request: BootNamespaceRequest<'request>,
        expected: &'expected [RetainedBootNamespaceExpectedSource<'source>],
        limits: RetainedBootNamespaceAssessmentLimits,
        deadline: Instant,
    ) -> Result<Self, RetainedBootNamespaceAssessmentError> {
        if expected.len() != 1 {
            return Err(RetainedBootNamespaceAssessmentError::ExpectedCountMismatch {
                expected: 1,
                found: expected.len(),
            });
        }
        let mut ledger = LiveLedger::new(limits, deadline)?;
        let mut evidence = bind_expected_streams(std::slice::from_ref(&request), expected, &mut ledger)?;
        let evidence = evidence.pop().ok_or(RetainedBootNamespaceAssessmentError::ObserverProtocol {
            reason: "single publication source binding omitted its evidence",
        })?;
        ledger.checkpoint()?;
        Ok(Self {
            request,
            source: &expected[0],
            evidence,
            ledger,
        })
    }

    pub(in crate::linux_fs) fn read_at(
        &mut self,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, RetainedBootNamespaceAssessmentError> {
        read_expected(
            self.source,
            self.evidence,
            self.request.expected_length(),
            offset,
            output,
            &mut self.ledger,
        )
    }

    pub(in crate::linux_fs) fn terminally_revalidate(
        &mut self,
    ) -> Result<(), RetainedBootNamespaceAssessmentError> {
        terminally_revalidate_expected_streams(
            std::slice::from_ref(&self.request),
            std::slice::from_ref(&self.source),
            std::slice::from_ref(&self.evidence),
            &mut self.ledger,
        )
    }

    pub(in crate::linux_fs) fn checkpoint(&self) -> Result<(), RetainedBootNamespaceAssessmentError> {
        self.ledger.checkpoint()
    }
}
