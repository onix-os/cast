//! What a reopened journal proves about a commit-cleanup advance.
//!
//! Persisting a cleanup advance closes the journal and reopens it. If the
//! advance itself failed — or its post-advance validation did — the only way to
//! learn whether the write actually landed is to read the reopened canonical
//! record and compare it against the two records that could legitimately be
//! there: the pre-advance source, or its successor.
//!
//! That comparison is the crash-safety decision, and it is identical for every
//! operation. Keeping it here means ActiveReblit and NewState cleanup share one
//! implementation rather than each restating it — a divergence between them
//! would mean one operation mis-reporting whether a durable write happened.
//! Error construction stays with each caller, which is where the operation's own
//! error vocabulary lives (`plans/future_impl.md` §1.1b).

use crate::transition_journal::TransitionRecord;

/// Which record a reopened journal proves durable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // consumed by the NewState cleanup persistence (§1.1b), then by ActiveReblit adoption
pub(in crate::client) enum ReopenedDurableRecord {
    /// The advance did not land; the pre-advance record is still current.
    Source,
    /// The advance landed; the successor is durable.
    Successor,
    /// Neither: a foreign record, or no record at all.
    Neither,
}

/// Classify the reopened canonical record against the only two records that may
/// legitimately be present after a cleanup advance.
///
/// `actual` is the record the reopened journal holds, if any. An absent record
/// is [`ReopenedDurableRecord::Neither`]: a cleanup advance never deletes the
/// journal, so emptiness is as foreign as an unrelated record.
#[allow(dead_code)] // consumed by the NewState cleanup persistence (§1.1b), then by ActiveReblit adoption
pub(in crate::client) fn classify_reopened_record(
    actual: Option<&TransitionRecord>,
    source: &TransitionRecord,
    successor: &TransitionRecord,
) -> ReopenedDurableRecord {
    match actual {
        Some(actual) if actual == source => ReopenedDurableRecord::Source,
        Some(actual) if actual == successor => ReopenedDurableRecord::Successor,
        _ => ReopenedDurableRecord::Neither,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        state::TransitionId,
        transition_journal::{
            BootId, MountNamespaceIdentity, Operation, Phase, Previous, PreviousOrigin, QuarantineName, RuntimeEpoch,
            RuntimeTreeIdentity, TreeToken,
        },
    };

    fn source_record() -> TransitionRecord {
        let mut record = TransitionRecord::preparing(
            TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            RuntimeEpoch {
                boot_id: BootId::parse("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
                mount_namespace: MountNamespaceIdentity { st_dev: 30, inode: 31 },
            },
            Operation::NewState,
            None,
            TreeToken::parse("a".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
            RuntimeTreeIdentity {
                st_dev: 10,
                inode: 10,
                mount_id: 12,
            },
            Previous {
                id: Some(41),
                tree_token: TreeToken::parse("b".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
                usr_runtime_identity: RuntimeTreeIdentity {
                    st_dev: 10,
                    inode: 20,
                    mount_id: 12,
                },
                origin: PreviousOrigin::ActiveState,
            },
            true,
            true,
            QuarantineName::parse("reopened-advance-test").unwrap(),
        )
        .unwrap();
        record.phase = Phase::CommitDecided;
        record.candidate.id = Some(42);
        record
    }

    fn successor_of(source: &TransitionRecord) -> TransitionRecord {
        let mut successor = source.clone();
        successor.phase = Phase::CommitCleanupComplete;
        successor.generation = source.generation + 1;
        successor
    }

    #[test]
    fn a_reopened_journal_distinguishes_a_landed_advance_from_a_lost_one() {
        let source = source_record();
        let successor = successor_of(&source);
        assert_ne!(source, successor);

        assert_eq!(
            classify_reopened_record(Some(&source), &source, &successor),
            ReopenedDurableRecord::Source,
            "the pre-advance record means the write never landed",
        );
        assert_eq!(
            classify_reopened_record(Some(&successor), &source, &successor),
            ReopenedDurableRecord::Successor,
            "the successor means the write landed despite the reported failure",
        );
    }

    #[test]
    fn an_absent_or_foreign_record_is_never_treated_as_durable() {
        let source = source_record();
        let successor = successor_of(&source);

        assert_eq!(
            classify_reopened_record(None, &source, &successor),
            ReopenedDurableRecord::Neither,
            "a cleanup advance never deletes the journal, so absence is foreign",
        );

        // A record of the right shape but the wrong generation is foreign: it is
        // neither the record advanced from nor the one advanced to.
        let mut skewed = successor.clone();
        skewed.generation += 1;
        assert_eq!(
            classify_reopened_record(Some(&skewed), &source, &successor),
            ReopenedDurableRecord::Neither,
        );

        // So is a different transition entirely.
        let mut other = source.clone();
        other.transition_id = TransitionId::parse("fedcba9876543210fedcba9876543210").unwrap();
        assert_eq!(
            classify_reopened_record(Some(&other), &source, &successor),
            ReopenedDurableRecord::Neither,
        );
    }
}
