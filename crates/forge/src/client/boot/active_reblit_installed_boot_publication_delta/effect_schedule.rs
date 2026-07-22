//! Closed, plan-ordered execution schedule from one sealed classified delta.
//!
//! The schedule can be constructed only from the opaque classified value. It
//! proves that every desired plan output consumed exactly one exact logical
//! root/path/byte entry and that every remaining classified entry is strictly
//! post-promotion stale work. It remains inert until the aggregate executor
//! pairs it with its private staged-receipt effect seal.

use std::collections::TryReserveError;

use thiserror::Error;

use crate::client::{
    active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
    active_reblit_publication_plan::ActiveReblitBootDestinationRoot,
};

use super::{
    ActiveReblitBootPublicationDeltaAction,
    ActiveReblitBootPublicationDeltaExpected,
    ClassifiedActiveReblitBootPublicationDelta,
};

/// One exact desired-output action in canonical plan order.
///
/// Fields are private so action scalars copied by unrelated callers can never
/// become an executable schedule.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) struct ActiveReblitBootPublicationEffectScheduleEntry {
    plan_index: usize,
    delta_index: usize,
    root: ActiveReblitBootDestinationRoot,
    action: ActiveReblitBootPublicationDeltaAction,
    desired_expected: ActiveReblitBootPublicationDeltaExpected,
    installed_expected: Option<ActiveReblitBootPublicationDeltaExpected>,
}

impl ActiveReblitBootPublicationEffectScheduleEntry {
    pub(in crate::client) const fn plan_index(&self) -> usize {
        self.plan_index
    }

    pub(in crate::client) const fn delta_index(&self) -> usize {
        self.delta_index
    }

    pub(in crate::client) const fn root(&self) -> ActiveReblitBootDestinationRoot {
        self.root
    }

    pub(in crate::client) const fn action(&self) -> ActiveReblitBootPublicationDeltaAction {
        self.action
    }

    pub(in crate::client) const fn desired_expected(
        &self,
    ) -> ActiveReblitBootPublicationDeltaExpected {
        self.desired_expected
    }

    pub(in crate::client) const fn installed_expected(
        &self,
    ) -> Option<ActiveReblitBootPublicationDeltaExpected> {
        self.installed_expected
    }
}

/// Complete desired-output schedule. This value is deliberately non-Clone.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) struct ActiveReblitBootPublicationEffectSchedule {
    entries: Vec<ActiveReblitBootPublicationEffectScheduleEntry>,
}

impl ActiveReblitBootPublicationEffectSchedule {
    pub(in crate::client) fn entries(
        &self,
    ) -> &[ActiveReblitBootPublicationEffectScheduleEntry] {
        &self.entries
    }
}

/// Closed failure while converting sealed classification into effect order.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootPublicationEffectScheduleError {
    #[error("allocate {resource}")]
    Allocation {
        resource: &'static str,
        #[source]
        source: TryReserveError,
    },
    #[error("desired plan output {plan_index} has a non-UTF-8 path")]
    NonUtf8DesiredPath { plan_index: usize },
    #[error("desired plan output {plan_index} has no exact classified root/path entry")]
    MissingDesiredEntry { plan_index: usize },
    #[error("desired plan output {plan_index} matches more than one classified root/path entry")]
    DuplicateDesiredEntry { plan_index: usize },
    #[error("classified entry {delta_index} does not retain the exact bytes for desired output {plan_index}")]
    DesiredIdentityMismatch {
        plan_index: usize,
        delta_index: usize,
    },
    #[error("classified entry {delta_index} for desired output {plan_index} has stale action {action:?}")]
    StaleActionForDesired {
        plan_index: usize,
        delta_index: usize,
        action: ActiveReblitBootPublicationDeltaAction,
    },
    #[error("owned retained output {plan_index} does not bind identical installed and desired bytes")]
    RetainedOwnedIdentityMismatch { plan_index: usize },
    #[error("owned replacement output {plan_index} lacks a distinct exact installed predecessor")]
    ReplacementIdentityMismatch { plan_index: usize },
    #[error("classified desired entry {delta_index} was not consumed by the canonical plan")]
    UnconsumedDesiredEntry { delta_index: usize },
    #[error("unconsumed classified entry {delta_index} has non-stale action {action:?}")]
    UnconsumedNonStaleAction {
        delta_index: usize,
        action: ActiveReblitBootPublicationDeltaAction,
    },
    #[error("stale classified entry {delta_index} lacks an exact installed identity")]
    StaleIdentityMissing { delta_index: usize },
}

impl ClassifiedActiveReblitBootPublicationDelta {
    /// Close this exact classified union into canonical desired plan order.
    pub(in crate::client) fn prepare_effect_schedule(
        &self,
        plan: &BoundActiveReblitBlsPublicationPlan<'_, '_, '_, '_, '_, '_>,
    ) -> Result<
        ActiveReblitBootPublicationEffectSchedule,
        ActiveReblitBootPublicationEffectScheduleError,
    > {
        let mut consumed = Vec::new();
        consumed.try_reserve_exact(self.entries.len()).map_err(|source| {
            ActiveReblitBootPublicationEffectScheduleError::Allocation {
                resource: "classified-delta consumption map",
                source,
            }
        })?;
        consumed.resize(self.entries.len(), false);

        let mut schedule = Vec::new();
        schedule
            .try_reserve_exact(plan.publication_count())
            .map_err(|source| ActiveReblitBootPublicationEffectScheduleError::Allocation {
                resource: "canonical boot effect schedule",
                source,
            })?;

        for (plan_index, output) in plan.outputs().enumerate() {
            let path = output.relative_path().to_str().ok_or(
                ActiveReblitBootPublicationEffectScheduleError::NonUtf8DesiredPath {
                    plan_index,
                },
            )?;
            let mut exact_match = None;
            for (delta_index, entry) in self.entries.iter().enumerate() {
                if entry.root == output.root()
                    && entry.relative_path.as_bytes() == path.as_bytes()
                {
                    if exact_match.replace(delta_index).is_some() {
                        return Err(
                            ActiveReblitBootPublicationEffectScheduleError::DuplicateDesiredEntry {
                                plan_index,
                            },
                        );
                    }
                }
            }
            let delta_index = exact_match.ok_or(
                ActiveReblitBootPublicationEffectScheduleError::MissingDesiredEntry {
                    plan_index,
                },
            )?;
            let entry = &self.entries[delta_index];
            let desired_expected = ActiveReblitBootPublicationDeltaExpected {
                checksum: output.expected_digest(),
                length: output.expected_length(),
                content_identity: output.expected_content_identity(),
            };
            if entry.desired_expected != Some(desired_expected) {
                return Err(
                    ActiveReblitBootPublicationEffectScheduleError::DesiredIdentityMismatch {
                        plan_index,
                        delta_index,
                    },
                );
            }
            validate_desired_action(
                plan_index,
                delta_index,
                entry.action,
                desired_expected,
                entry.installed_expected,
            )?;
            consumed[delta_index] = true;
            schedule.push(ActiveReblitBootPublicationEffectScheduleEntry {
                plan_index,
                delta_index,
                root: entry.root,
                action: entry.action,
                desired_expected,
                installed_expected: entry.installed_expected,
            });
        }

        for (delta_index, (entry, consumed)) in
            self.entries.iter().zip(consumed).enumerate()
        {
            if consumed {
                continue;
            }
            if entry.desired_expected.is_some() {
                return Err(
                    ActiveReblitBootPublicationEffectScheduleError::UnconsumedDesiredEntry {
                        delta_index,
                    },
                );
            }
            if !matches!(
                entry.action,
                ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion
                    | ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale
            ) {
                return Err(
                    ActiveReblitBootPublicationEffectScheduleError::UnconsumedNonStaleAction {
                        delta_index,
                        action: entry.action,
                    },
                );
            }
            if entry.installed_expected.is_none() {
                return Err(
                    ActiveReblitBootPublicationEffectScheduleError::StaleIdentityMissing {
                        delta_index,
                    },
                );
            }
        }

        Ok(ActiveReblitBootPublicationEffectSchedule { entries: schedule })
    }
}

fn validate_desired_action(
    plan_index: usize,
    delta_index: usize,
    action: ActiveReblitBootPublicationDeltaAction,
    desired: ActiveReblitBootPublicationDeltaExpected,
    installed: Option<ActiveReblitBootPublicationDeltaExpected>,
) -> Result<(), ActiveReblitBootPublicationEffectScheduleError> {
    match action {
        ActiveReblitBootPublicationDeltaAction::PublishDesired
        | ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired => Ok(()),
        ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired => {
            if installed == Some(desired) {
                Ok(())
            } else {
                Err(
                    ActiveReblitBootPublicationEffectScheduleError::RetainedOwnedIdentityMismatch {
                        plan_index,
                    },
                )
            }
        }
        ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired => {
            if installed.is_some_and(|installed| installed != desired) {
                Ok(())
            } else {
                Err(
                    ActiveReblitBootPublicationEffectScheduleError::ReplacementIdentityMismatch {
                        plan_index,
                    },
                )
            }
        }
        ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion
        | ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale => Err(
            ActiveReblitBootPublicationEffectScheduleError::StaleActionForDesired {
                plan_index,
                delta_index,
                action,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::boot_content_identity::BootContentIdentity;

    fn expected(byte: u8) -> ActiveReblitBootPublicationDeltaExpected {
        ActiveReblitBootPublicationDeltaExpected {
            checksum: u128::from(byte),
            length: u64::from(byte),
            content_identity: BootContentIdentity::from_sha256([byte; 32]),
        }
    }

    #[test]
    fn closed_desired_dispatch_requires_exact_old_and_new_identities() {
        let old = expected(1);
        let new = expected(2);
        assert!(validate_desired_action(
            0,
            4,
            ActiveReblitBootPublicationDeltaAction::PublishDesired,
            new,
            None,
        )
        .is_ok());
        assert!(validate_desired_action(
            1,
            5,
            ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired,
            new,
            Some(old),
        )
        .is_ok());
        assert!(validate_desired_action(
            2,
            6,
            ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired,
            new,
            Some(new),
        )
        .is_ok());
        assert!(validate_desired_action(
            3,
            7,
            ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired,
            new,
            Some(old),
        )
        .is_ok());
    }

    #[test]
    fn stale_and_ambiguous_identity_actions_never_enter_desired_schedule() {
        let exact = expected(9);
        assert!(matches!(
            validate_desired_action(
                1,
                2,
                ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion,
                exact,
                Some(exact),
            ),
            Err(ActiveReblitBootPublicationEffectScheduleError::StaleActionForDesired {
                ..
            }),
        ));
        assert!(matches!(
            validate_desired_action(
                1,
                2,
                ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired,
                exact,
                Some(exact),
            ),
            Err(
                ActiveReblitBootPublicationEffectScheduleError::ReplacementIdentityMismatch {
                    ..
                }
            ),
        ));
    }
}
