use std::{cmp::Ordering, path::PathBuf, time::Instant};

use super::{
    super::active_reblit_publication_plan::ActiveReblitBootPublicationSource, ActiveReblitBlsRendererError,
    BoundActiveReblitBootAsset, RenderBudget, allocation,
};

pub(super) struct PayloadCandidate<'asset> {
    pub(super) path: PathBuf,
    pub(super) binding_index: u16,
    pub(super) digest: u128,
    pub(super) length: u64,
    pub(super) asset: Option<BoundActiveReblitBootAsset<'asset>>,
}

pub(super) struct RetainedSealedSource<'asset> {
    binding_index: u16,
    digest: u128,
    length: u64,
    asset: BoundActiveReblitBootAsset<'asset>,
}

pub(super) struct SealedSourceCatalog<'asset> {
    sources: Box<[RetainedSealedSource<'asset>]>,
}

impl<'asset> PayloadCandidate<'asset> {
    pub(super) fn take_source(&mut self) -> RetainedSealedSource<'asset> {
        RetainedSealedSource {
            binding_index: self.binding_index,
            digest: self.digest,
            length: self.length,
            asset: self
                .asset
                .take()
                .expect("canonical payload retains its exact aggregate asset view"),
        }
    }
}

impl<'asset> RetainedSealedSource<'asset> {
    pub(super) const fn new(
        binding_index: u16,
        digest: u128,
        length: u64,
        asset: BoundActiveReblitBootAsset<'asset>,
    ) -> Self {
        Self {
            binding_index,
            digest,
            length,
            asset,
        }
    }

    fn key(&self) -> (u16, u128, u64) {
        (self.binding_index, self.digest, self.length)
    }
}

impl<'asset> SealedSourceCatalog<'asset> {
    pub(super) fn prepare(
        systemd_boot: RetainedSealedSource<'asset>,
        payloads: &mut [PayloadCandidate<'asset>],
        budget: &mut RenderBudget,
    ) -> Result<Self, ActiveReblitBlsRendererError> {
        budget.reserve_sort_work(payloads.len().saturating_add(1))?;
        let mut sources = Vec::new();
        sources
            .try_reserve_exact(payloads.len().saturating_add(1))
            .map_err(|source| allocation("sealed BLS source catalog", source))?;
        sources.push(systemd_boot);
        sources.extend(payloads.iter_mut().map(PayloadCandidate::take_source));
        sources.sort_unstable_by_key(RetainedSealedSource::key);
        sources.dedup_by(|left, right| left.key() == right.key());
        for _ in &sources {
            budget.step()?;
        }
        budget.require_deadline("sealed source catalog completion")?;
        Ok(Self {
            sources: sources.into_boxed_slice(),
        })
    }

    pub(super) fn contains_publication_source(&self, source: &ActiveReblitBootPublicationSource) -> bool {
        let ActiveReblitBootPublicationSource::SealedSnapshot {
            binding_index,
            digest,
            length,
        } = source
        else {
            return false;
        };
        self.asset_for_key((*binding_index, *digest, *length)).is_some()
    }

    pub(super) fn asset_for_publication_source(
        &self,
        source: &ActiveReblitBootPublicationSource,
    ) -> Option<&BoundActiveReblitBootAsset<'asset>> {
        let ActiveReblitBootPublicationSource::SealedSnapshot {
            binding_index,
            digest,
            length,
        } = source
        else {
            return None;
        };
        self.asset_for_key((*binding_index, *digest, *length))
            .map(|retained| &retained.asset)
    }

    fn asset_for_key(&self, key: (u16, u128, u64)) -> Option<&RetainedSealedSource<'asset>> {
        self.sources
            .binary_search_by_key(&key, RetainedSealedSource::key)
            .ok()
            .map(|index| &self.sources[index])
    }
}

pub(super) fn canonicalize_payloads<'asset, N>(
    mut candidates: Vec<PayloadCandidate<'asset>>,
    budget: &mut RenderBudget,
    post_sort_now: N,
) -> Result<Vec<PayloadCandidate<'asset>>, ActiveReblitBlsRendererError>
where
    N: FnOnce() -> Instant,
{
    budget.reserve_sort_work(candidates.len())?;
    candidates.sort_unstable_by(compare_candidate);
    budget.require_deadline_at("payload sort completion", post_sort_now())?;

    let mut canonical: Vec<PayloadCandidate<'asset>> = Vec::new();
    canonical
        .try_reserve_exact(candidates.len())
        .map_err(|source| allocation("canonical BLS payloads", source))?;
    for candidate in candidates {
        budget.step()?;
        let Some(previous) = canonical.last() else {
            canonical.push(candidate);
            continue;
        };
        let previous_path = previous
            .path
            .to_str()
            .expect("renderer-created payload paths are validated ASCII");
        let candidate_path = candidate
            .path
            .to_str()
            .expect("renderer-created payload paths are validated ASCII");
        if !previous_path.eq_ignore_ascii_case(candidate_path) {
            canonical.push(candidate);
            continue;
        }
        if previous_path != candidate_path {
            return Err(ActiveReblitBlsRendererError::PayloadCaseCollision {
                first: previous.path.clone(),
                second: candidate.path,
            });
        }
        if previous.digest != candidate.digest || previous.length != candidate.length {
            return Err(ActiveReblitBlsRendererError::PayloadCollision { path: candidate.path });
        }
        // Sorting makes the smallest exact binding coordinate canonical.
        debug_assert!(previous.binding_index <= candidate.binding_index);
    }
    Ok(canonical)
}

fn compare_candidate(left: &PayloadCandidate<'_>, right: &PayloadCandidate<'_>) -> Ordering {
    let left_path = left
        .path
        .to_str()
        .expect("renderer-created payload paths are validated ASCII");
    let right_path = right
        .path
        .to_str()
        .expect("renderer-created payload paths are validated ASCII");
    ascii_fold_cmp(left_path.as_bytes(), right_path.as_bytes())
        .then_with(|| left_path.cmp(right_path))
        .then_with(|| left.digest.cmp(&right.digest))
        .then_with(|| left.length.cmp(&right.length))
        .then_with(|| left.binding_index.cmp(&right.binding_index))
}

pub(super) fn ascii_fold_cmp(left: &[u8], right: &[u8]) -> Ordering {
    left.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(right.iter().map(u8::to_ascii_lowercase))
}
