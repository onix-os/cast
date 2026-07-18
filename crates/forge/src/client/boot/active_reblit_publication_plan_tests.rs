use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use super::*;

fn payload(
    path: impl Into<PathBuf>,
    binding_index: u16,
    digest: u128,
    length: u64,
) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::sealed_payload(path.into(), binding_index, digest, length)
}

fn entry(path: impl Into<PathBuf>, bytes: &[u8]) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::generated_entry(path.into(), bytes.into())
}

fn loader_control(bytes: &[u8]) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::generated_loader_control(bytes.into())
}

fn fallback_bootloader(binding_index: u16, digest: u128, length: u64) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::sealed_fallback_bootloader(binding_index, digest, length)
}

fn systemd_bootloader(binding_index: u16, digest: u128, length: u64) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::sealed_systemd_bootloader(binding_index, digest, length)
}

fn sealed_source(binding_index: u16, digest: u128, length: u64) -> ActiveReblitBootPublicationRequestSource {
    ActiveReblitBootPublicationRequestSource::SealedSnapshot {
        binding_index,
        digest,
        length,
    }
}

fn generated_source(bytes: &[u8]) -> ActiveReblitBootPublicationRequestSource {
    ActiveReblitBootPublicationRequestSource::Generated { bytes: bytes.into() }
}

fn prepare_with_policy(
    requests: impl IntoIterator<Item = ActiveReblitBootPublicationRequest>,
    policy: PublicationPlanPolicy,
) -> Result<PreparedActiveReblitBootPublicationPlan, ActiveReblitBootPublicationPlanError> {
    prepare_publication_plan(requests, policy, Some(Instant::now() + Duration::from_secs(5)))
}

include!("active_reblit_publication_plan_tests/roles_and_collisions.rs");
include!("active_reblit_publication_plan_tests/path_policy.rs");
include!("active_reblit_publication_plan_tests/bounds_and_deadlines.rs");
