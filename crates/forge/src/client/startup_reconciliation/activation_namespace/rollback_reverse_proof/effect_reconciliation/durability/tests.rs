use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
    path::Path,
};

use crate::{
    Installation,
    state::TransitionId,
    test_support::private_installation_tempdir,
    transition_identity::{
        RetainedExchangeSyscallFault, arm_retained_exchange_syscall_fault, reset_retained_exchange_syscall_count,
        retained_exchange_syscall_count,
    },
    transition_journal::{
        InitialRollbackAction, Operation, Phase, Previous, PreviousOrigin, QuarantineName, RollbackObservations,
        RuntimeEpoch, RuntimeTreeIdentity, TransitionRecord, TreeToken,
    },
    tree_marker::TreeMarkerStore,
};

use super::super::super::UsrRollbackReverseNamespaceEffectEvidence;
use super::super::{
    UsrRollbackReverseAlreadySatisfiedNamespace, UsrRollbackReverseAppliedNamespace,
    UsrRollbackReverseNamespaceApplyReconciliation,
};
use super::*;
use crate::client::startup_reconciliation::activation_namespace::{
    capture::{ProjectedReverseNamespace, RetainedReverseExchangeParents, capture_snapshot},
    policy::UsrExchangeLayout,
};

const ROOT_ABI: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

#[derive(Clone, Copy, Debug)]
enum ReconciliationSource {
    AppliedSuccess,
    AppliedReportedError,
    AlreadySatisfied,
}

struct DurabilityFixture {
    _temporary: tempfile::TempDir,
    installation: Installation,
    record: TransitionRecord,
}

enum PendingDurability {
    Applied(UsrRollbackReverseAppliedNamespace),
    AlreadySatisfied(UsrRollbackReverseAlreadySatisfiedNamespace),
}

impl DurabilityFixture {
    fn new(source: ReconciliationSource) -> Self {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        install_isolation_abi(&installation.root);
        let (candidate_token, candidate_runtime) = create_marked_tree(&installation.root.join("usr"));
        let (previous_token, previous_runtime) = create_marked_tree(&installation.staging_path("usr"));
        let record = reverse_intent_record(candidate_token, candidate_runtime, previous_token, previous_runtime);
        if matches!(source, ReconciliationSource::AlreadySatisfied) {
            exchange_usr_layout(&installation.root);
        }
        Self {
            _temporary: temporary,
            installation,
            record,
        }
    }

    fn pending(&self, source: ReconciliationSource) -> PendingDurability {
        let evidence = self.effect_evidence();
        match source {
            ReconciliationSource::AppliedSuccess | ReconciliationSource::AppliedReportedError => {
                if matches!(source, ReconciliationSource::AppliedReportedError) {
                    arm_retained_exchange_syscall_fault(RetainedExchangeSyscallFault::ErrorAfterApply);
                } else {
                    reset_retained_exchange_syscall_count();
                }
                let result = evidence.reconcile_apply(&self.installation, &self.record).unwrap();
                let UsrRollbackReverseNamespaceApplyReconciliation::Applied(applied) = result else {
                    panic!("exact POST-to-PRE effect did not yield applied evidence");
                };
                assert_eq!(retained_exchange_syscall_count(), 1);
                PendingDurability::Applied(applied)
            }
            ReconciliationSource::AlreadySatisfied => {
                reset_retained_exchange_syscall_count();
                let satisfied = evidence.reconcile_finish(&self.installation, &self.record).unwrap();
                assert_eq!(retained_exchange_syscall_count(), 0);
                PendingDurability::AlreadySatisfied(satisfied)
            }
        }
    }

    fn effect_evidence(&self) -> UsrRollbackReverseNamespaceEffectEvidence {
        let baseline = capture_snapshot(&self.installation, &self.record).unwrap();
        let projection = ProjectedReverseNamespace::capture(&baseline, &self.record).unwrap();
        let parents = RetainedReverseExchangeParents::capture(&baseline, &self.record).unwrap();
        let layout = projection.layout();
        UsrRollbackReverseNamespaceEffectEvidence {
            baseline,
            projection,
            parents,
            layout,
        }
    }

    fn expected_events(&self) -> Vec<UsrRollbackReverseNamespaceDurabilityEvent> {
        let (staging_device, staging_inode) = identity(&self.installation.root.join(".cast/root/staging"));
        let (root_device, root_inode) = identity(&self.installation.root);
        vec![
            UsrRollbackReverseNamespaceDurabilityEvent::StagingParentSynced {
                device: staging_device,
                inode: staging_inode,
            },
            UsrRollbackReverseNamespaceDurabilityEvent::InstallationRootSynced {
                device: root_device,
                inode: root_inode,
            },
            UsrRollbackReverseNamespaceDurabilityEvent::FinalPreProven,
        ]
    }

    fn namespace_change_hook(&self, name: &str) -> impl FnOnce() + 'static {
        let path = self.installation.state_quarantine_dir().join(name);
        move || {
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
}

impl PendingDurability {
    fn complete(
        self,
        fixture: &DurabilityFixture,
    ) -> Result<UsrRollbackReverseDurableNamespace, super::super::UsrRollbackReverseNamespaceError> {
        match self {
            Self::Applied(applied) => applied.complete_parent_durability(&fixture.installation, &fixture.record),
            Self::AlreadySatisfied(satisfied) => {
                satisfied.complete_parent_durability(&fixture.installation, &fixture.record)
            }
        }
    }
}

#[test]
fn reverse_parent_durability_syncs_staging_then_root_then_proves_pre_for_both_sources() {
    for source in [
        ReconciliationSource::AppliedSuccess,
        ReconciliationSource::AppliedReportedError,
        ReconciliationSource::AlreadySatisfied,
    ] {
        let fixture = DurabilityFixture::new(source);
        let pending = fixture.pending(source);
        let expected_calls = retained_exchange_syscall_count();
        let expected_events = fixture.expected_events();
        reset_usr_rollback_reverse_namespace_durability_events();

        let _durable = pending.complete(&fixture).unwrap();

        assert_eq!(
            take_usr_rollback_reverse_namespace_durability_events(),
            expected_events,
            "{source:?}"
        );
        assert_eq!(retained_exchange_syscall_count(), expected_calls, "{source:?}");
        assert_eq!(
            ProjectedReverseNamespace::capture(
                &capture_snapshot(&fixture.installation, &fixture.record).unwrap(),
                &fixture.record,
            )
            .unwrap()
            .layout(),
            UsrExchangeLayout::Pre,
        );
    }
}

#[test]
fn reverse_parent_durability_faults_stop_at_the_exact_event_prefix_for_both_sources() {
    let cases = [
        (UsrRollbackReverseNamespaceDurabilityFaultPoint::StagingParentSync, 0),
        (UsrRollbackReverseNamespaceDurabilityFaultPoint::InstallationRootSync, 1),
        (UsrRollbackReverseNamespaceDurabilityFaultPoint::FinalPreCapture, 2),
    ];
    for source in [
        ReconciliationSource::AppliedSuccess,
        ReconciliationSource::AlreadySatisfied,
    ] {
        for (fault, expected_prefix_len) in cases {
            let fixture = DurabilityFixture::new(source);
            let pending = fixture.pending(source);
            let expected_calls = retained_exchange_syscall_count();
            let expected_events = fixture.expected_events();
            reset_usr_rollback_reverse_namespace_durability_events();
            arm_usr_rollback_reverse_namespace_durability_fault(fault);

            assert!(pending.complete(&fixture).is_err(), "{source:?} {fault:?}");

            assert_eq!(
                take_usr_rollback_reverse_namespace_durability_events(),
                expected_events[..expected_prefix_len],
                "{source:?} {fault:?}"
            );
            assert_eq!(
                retained_exchange_syscall_count(),
                expected_calls,
                "{source:?} {fault:?}"
            );
        }
    }
}

#[test]
fn reverse_parent_durability_between_sync_namespace_race_prevents_root_sync() {
    for source in [
        ReconciliationSource::AppliedSuccess,
        ReconciliationSource::AlreadySatisfied,
    ] {
        let fixture = DurabilityFixture::new(source);
        let pending = fixture.pending(source);
        let expected_events = fixture.expected_events();
        reset_usr_rollback_reverse_namespace_durability_events();
        arm_before_usr_rollback_reverse_namespace_installation_root_sync(
            fixture.namespace_change_hook("reverse-durability-between-sync-race"),
        );

        assert!(pending.complete(&fixture).is_err(), "{source:?}");
        assert_eq!(
            take_usr_rollback_reverse_namespace_durability_events(),
            expected_events[..1],
            "{source:?}"
        );
    }
}

#[test]
fn reverse_parent_durability_final_fresh_pre_race_rejects_completion_after_both_syncs() {
    for source in [
        ReconciliationSource::AppliedSuccess,
        ReconciliationSource::AlreadySatisfied,
    ] {
        let fixture = DurabilityFixture::new(source);
        let pending = fixture.pending(source);
        let expected_events = fixture.expected_events();
        reset_usr_rollback_reverse_namespace_durability_events();
        arm_before_usr_rollback_reverse_namespace_final_pre_capture(
            fixture.namespace_change_hook("reverse-durability-final-pre-race"),
        );

        assert!(pending.complete(&fixture).is_err(), "{source:?}");
        assert_eq!(
            take_usr_rollback_reverse_namespace_durability_events(),
            expected_events[..2],
            "{source:?}"
        );
    }
}

fn reverse_intent_record(
    candidate_token: TreeToken,
    candidate_runtime: RuntimeTreeIdentity,
    previous_token: TreeToken,
    previous_runtime: RuntimeTreeIdentity,
) -> TransitionRecord {
    let mut record = TransitionRecord::preparing(
        TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        RuntimeEpoch::capture().unwrap(),
        Operation::NewState,
        None,
        candidate_token,
        candidate_runtime,
        Previous {
            id: None,
            tree_token: previous_token,
            usr_runtime_identity: previous_runtime,
            origin: PreviousOrigin::SynthesizedEmpty,
        },
        true,
        true,
        QuarantineName::parse("failed-reverse-parent-durability").unwrap(),
    )
    .unwrap();
    while record.phase != Phase::UsrExchanged {
        let allocated = (record.phase == Phase::FreshStateAllocating).then_some(73);
        record = record.forward_successor(allocated).unwrap();
    }
    let decision = record
        .rollback_decision(RollbackObservations {
            allocated_candidate_id: None,
            previous_archive: None,
            usr_exchange: Some(InitialRollbackAction::Pending),
            candidate: InitialRollbackAction::Pending,
            fresh_db: Some(InitialRollbackAction::Pending),
        })
        .unwrap();
    let reverse = decision.rollback_successor(None).unwrap();
    assert_eq!(reverse.phase, Phase::ReverseExchangeIntent);
    reverse
}

fn create_marked_tree(path: &Path) -> (TreeToken, RuntimeTreeIdentity) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    let store = TreeMarkerStore::open_path(path).unwrap();
    let marker = store.adopt_or_create_before_journal().unwrap();
    let token = marker.token().clone();
    let runtime = RuntimeTreeIdentity::capture_directory(store.retained_directory()).unwrap();
    (token, runtime)
}

fn install_isolation_abi(root: &Path) {
    for (name, target) in ROOT_ABI {
        symlink(target, root.join(".cast/root/isolation").join(name)).unwrap();
    }
}

fn exchange_usr_layout(root: &Path) {
    let live = root.join("usr");
    let staging = root.join(".cast/root/staging/usr");
    let parked = root.join(".cast/root/.reverse-durability-parked");
    fs::rename(&live, &parked).unwrap();
    fs::rename(&staging, &live).unwrap();
    fs::rename(&parked, &staging).unwrap();
}

fn identity(path: &Path) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}
