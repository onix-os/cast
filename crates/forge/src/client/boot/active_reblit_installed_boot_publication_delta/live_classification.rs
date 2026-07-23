//! Production classification from one unforgeable retained-preflight seal.

use std::{collections::BTreeMap, os::unix::ffi::OsStrExt as _};

use crate::{
    client::active_reblit_boot_publication_preflight::{
        ActiveReblitBootPublicationAssessmentSeal,
        SealedActiveReblitBootPublicationDesiredState,
    },
    linux_fs::descriptor_boot_namespace::BootNamespaceDestinationState,
};

use super::{
    ActiveReblitBootPublicationDeltaAction,
    ActiveReblitBootPublicationDeltaError,
    ActiveReblitBootPublicationDeltaExpected,
    ActiveReblitBootPublicationDeltaRequest,
    ClassifiedActiveReblitBootPublicationDelta,
    ClassifiedActiveReblitBootPublicationDeltaEntry,
    PreparedActiveReblitBootPublicationDelta,
    clone_text,
};

impl PreparedActiveReblitBootPublicationDelta {
    pub(in crate::client) fn classify_with_preflight_assessment(
        &self,
        seal: &ActiveReblitBootPublicationAssessmentSeal<'_>,
    ) -> Result<ClassifiedActiveReblitBootPublicationDelta, ActiveReblitBootPublicationDeltaError> {
        if self.destination_layout != seal.destination_layout() {
            return Err(
                ActiveReblitBootPublicationDeltaError::PreflightDestinationLayoutMismatch,
            );
        }
        let desired_request_count = self
            .requests
            .iter()
            .filter(|request| request.desired.is_some())
            .count();
        if desired_request_count != seal.desired_states().len() {
            return Err(
                ActiveReblitBootPublicationDeltaError::PreflightDesiredCountMismatch {
                    expected: seal.desired_states().len(),
                    actual: desired_request_count,
                },
            );
        }

        let mut sealed_by_key = BTreeMap::new();
        for (plan_index, sealed) in seal.desired_states().iter().enumerate() {
            let key = (
                sealed.root(),
                sealed.relative_path().as_os_str().as_bytes(),
            );
            if sealed_by_key.insert(key, (plan_index, sealed)).is_some() {
                return Err(
                    ActiveReblitBootPublicationDeltaError::DuplicatePreflightDesiredKey,
                );
            }
        }

        let mut entries = Vec::new();
        entries.try_reserve_exact(self.requests.len()).map_err(|source| {
            ActiveReblitBootPublicationDeltaError::Allocation {
                resource: "sealed preflight classified delta entries",
                source,
            }
        })?;
        for (index, request) in self.requests.iter().enumerate() {
            let action = if let Some(expected) = request.desired {
                let key = (request.root, request.relative_path.as_bytes());
                let Some((_, sealed)) = sealed_by_key.remove(&key) else {
                    return Err(
                        ActiveReblitBootPublicationDeltaError::MissingPreflightDesiredKey {
                            index,
                        },
                    );
                };
                if !sealed_expected_matches(sealed, expected) {
                    return Err(
                        ActiveReblitBootPublicationDeltaError::PreflightDesiredExpectationMismatch {
                            index,
                        },
                    );
                }
                classify_desired(index, request, sealed.state())?
            } else {
                classify_stale(index, request)?
            };
            entries.push(ClassifiedActiveReblitBootPublicationDeltaEntry {
                root: request.root,
                relative_path: clone_text(
                    &request.relative_path,
                    "sealed classified relative path",
                )?,
                desired_expected: request.desired,
                installed_expected: request.installed,
                action,
            });
        }

        if let Some((_, (plan_index, _))) = sealed_by_key.into_iter().next() {
            return Err(
                ActiveReblitBootPublicationDeltaError::UnmatchedPreflightDesiredKey {
                    plan_index,
                },
            );
        }
        Ok(ClassifiedActiveReblitBootPublicationDelta { entries })
    }
}

fn sealed_expected_matches(
    sealed: &SealedActiveReblitBootPublicationDesiredState<'_>,
    expected: ActiveReblitBootPublicationDeltaExpected,
) -> bool {
    sealed.checksum() == expected.checksum
        && sealed.length() == expected.length
        && sealed.content_identity() == expected.content_identity
}

fn classify_desired(
    index: usize,
    request: &ActiveReblitBootPublicationDeltaRequest,
    state: BootNamespaceDestinationState,
) -> Result<ActiveReblitBootPublicationDeltaAction, ActiveReblitBootPublicationDeltaError> {
    if request.installed_owned && request.installed.is_none() {
        return Err(
            ActiveReblitBootPublicationDeltaError::OwnedOutputWithoutInstalledIdentity {
                index,
            },
        );
    }
    match state {
        BootNamespaceDestinationState::Absent => {
            Ok(ActiveReblitBootPublicationDeltaAction::PublishDesired)
        }
        BootNamespaceDestinationState::Exact if request.installed_owned => {
            Ok(ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired)
        }
        BootNamespaceDestinationState::Exact => {
            Ok(ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired)
        }
        BootNamespaceDestinationState::Different if request.installed_owned => {
            Ok(ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired)
        }
        BootNamespaceDestinationState::Different => {
            Err(ActiveReblitBootPublicationDeltaError::UnownedDifferentDesired {
                index,
            })
        }
    }
}

fn classify_stale(
    index: usize,
    request: &ActiveReblitBootPublicationDeltaRequest,
) -> Result<ActiveReblitBootPublicationDeltaAction, ActiveReblitBootPublicationDeltaError> {
    if request.installed.is_none() {
        return Err(
            ActiveReblitBootPublicationDeltaError::OwnedOutputWithoutInstalledIdentity {
                index,
            },
        );
    }
    if request.installed_owned {
        Ok(ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion)
    } else {
        Ok(ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale)
    }
}

#[cfg(test)]
pub(super) fn classify_desired_for_test(
    index: usize,
    request: &ActiveReblitBootPublicationDeltaRequest,
    state: BootNamespaceDestinationState,
) -> Result<ActiveReblitBootPublicationDeltaAction, ActiveReblitBootPublicationDeltaError> {
    classify_desired(index, request, state)
}

#[cfg(test)]
pub(super) fn classify_stale_for_test(
    index: usize,
    request: &ActiveReblitBootPublicationDeltaRequest,
) -> Result<ActiveReblitBootPublicationDeltaAction, ActiveReblitBootPublicationDeltaError> {
    classify_stale(index, request)
}
