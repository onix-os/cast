use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
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
        Operation, Previous, PreviousOrigin, QuarantineName, RuntimeEpoch, RuntimeTreeIdentity, TransitionRecord,
        TreeToken,
    },
    tree_marker::TreeMarkerStore,
};

use super::*;
use crate::client::startup_reconciliation::activation_namespace::capture::{
    ProjectedReverseNamespace, RetainedReverseExchangeParents, capture_snapshot,
};

struct ReconciliationFixture {
    _temporary: tempfile::TempDir,
    installation: Installation,
    record: TransitionRecord,
}

impl ReconciliationFixture {
    fn post() -> Self {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let (candidate_token, candidate_runtime) = create_marked_tree(&installation.root.join("usr"));
        let (previous_token, previous_runtime) = create_marked_tree(&installation.staging_path("usr"));
        let record = TransitionRecord::preparing(
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
            QuarantineName::parse("failed-reverse-effect-reconciliation").unwrap(),
        )
        .unwrap();
        Self {
            _temporary: temporary,
            installation,
            record,
        }
    }

    fn baseline(
        &self,
    ) -> (
        NamespaceSnapshot,
        ProjectedReverseNamespace,
        RetainedReverseExchangeParents,
    ) {
        let baseline = capture_snapshot(&self.installation, &self.record).unwrap();
        let projection = ProjectedReverseNamespace::capture(&baseline, &self.record).unwrap();
        assert_eq!(projection.layout(), UsrExchangeLayout::Post);
        let parents = RetainedReverseExchangeParents::capture(&baseline, &self.record).unwrap();
        (baseline, projection, parents)
    }

    fn reconcile(&self, fault: Option<RetainedExchangeSyscallFault>) -> ReverseExchangeReconciliation {
        let (baseline, projection, parents) = self.baseline();
        arm_fault(fault);
        let hook_runs = Arc::new(AtomicUsize::new(0));
        let hook_observer = Arc::clone(&hook_runs);
        arm_before_reverse_exchange_reconciliation_capture(move || {
            hook_observer.fetch_add(1, Ordering::SeqCst);
        });

        let result =
            parents
                .attempt_usr_exchange_once()
                .reconcile(&self.installation, &self.record, baseline, projection);

        assert_eq!(retained_exchange_syscall_count(), 1);
        assert_eq!(hook_runs.load(Ordering::SeqCst), 1);
        result
    }
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

fn arm_fault(fault: Option<RetainedExchangeSyscallFault>) {
    if let Some(fault) = fault {
        arm_retained_exchange_syscall_fault(fault);
    } else {
        reset_retained_exchange_syscall_count();
    }
}

fn expect_applied(result: ReverseExchangeReconciliation, raw_success: bool, record: &TransitionRecord) {
    let ReverseExchangeReconciliation::Applied(applied) = result else {
        panic!("exact PRE namespace was not classified as applied");
    };
    assert_eq!(applied.fresh_pre_projection.layout(), UsrExchangeLayout::Pre);
    assert_eq!(applied.raw_report.is_ok(), raw_success);
    assert!(applied.parents.root.metadata().is_ok());
    assert_eq!(
        ProjectedReverseNamespace::capture(&applied.fresh_pre, record).unwrap(),
        applied.fresh_pre_projection
    );
}

fn expect_not_applied(result: ReverseExchangeReconciliation) {
    assert!(matches!(result, ReverseExchangeReconciliation::NotApplied));
}

fn expect_ambiguous(result: ReverseExchangeReconciliation) {
    assert!(matches!(result, ReverseExchangeReconciliation::Ambiguous));
}

#[test]
fn reverse_exchange_reconciliation_classifies_normal_success_from_fresh_pre_evidence() {
    let fixture = ReconciliationFixture::post();
    expect_applied(fixture.reconcile(None), true, &fixture.record);
}

#[test]
fn reverse_exchange_reconciliation_classifies_reported_error_from_fresh_pre_evidence() {
    let fixture = ReconciliationFixture::post();
    expect_applied(
        fixture.reconcile(Some(RetainedExchangeSyscallFault::ErrorAfterApply)),
        false,
        &fixture.record,
    );
}

#[test]
fn reverse_exchange_reconciliation_classifies_reported_success_only_from_exact_unchanged_post() {
    let fixture = ReconciliationFixture::post();
    expect_not_applied(fixture.reconcile(Some(RetainedExchangeSyscallFault::SuccessWithoutApply)));
}

#[test]
fn reverse_exchange_reconciliation_classifies_reported_error_only_from_exact_unchanged_post() {
    let fixture = ReconciliationFixture::post();
    expect_not_applied(fixture.reconcile(Some(RetainedExchangeSyscallFault::ErrorWithoutApply)));
}

#[test]
fn reverse_exchange_reconciliation_maps_fresh_capture_failure_to_ambiguous() {
    let fixture = ReconciliationFixture::post();
    let (baseline, projection, parents) = fixture.baseline();
    let live_marker = fixture.installation.root.join("usr/.cast-tree-id");
    arm_before_reverse_exchange_reconciliation_capture(move || fs::remove_file(live_marker).unwrap());
    reset_retained_exchange_syscall_count();

    let result =
        parents
            .attempt_usr_exchange_once()
            .reconcile(&fixture.installation, &fixture.record, baseline, projection);

    assert_eq!(retained_exchange_syscall_count(), 1);
    expect_ambiguous(result);
}

#[test]
fn reverse_exchange_reconciliation_maps_extra_namespace_delta_to_ambiguous() {
    let fixture = ReconciliationFixture::post();
    let (baseline, projection, parents) = fixture.baseline();
    let ambient = fixture
        .installation
        .state_quarantine_dir()
        .join("ambient-reconciliation-delta");
    arm_before_reverse_exchange_reconciliation_capture(move || {
        fs::create_dir(&ambient).unwrap();
        fs::set_permissions(&ambient, fs::Permissions::from_mode(0o700)).unwrap();
    });
    reset_retained_exchange_syscall_count();

    let result =
        parents
            .attempt_usr_exchange_once()
            .reconcile(&fixture.installation, &fixture.record, baseline, projection);

    assert_eq!(retained_exchange_syscall_count(), 1);
    expect_ambiguous(result);
}

#[test]
fn reverse_exchange_reconciliation_maps_foreign_mixed_layout_to_ambiguous() {
    let fixture = ReconciliationFixture::post();
    let (baseline, projection, parents) = fixture.baseline();
    let root = fixture.installation.root.clone();
    let ambient = fixture.installation.state_quarantine_dir().join("ambient-mixed-layout");
    arm_before_reverse_exchange_reconciliation_capture(move || {
        fs::create_dir(&ambient).unwrap();
        fs::set_permissions(&ambient, fs::Permissions::from_mode(0o700)).unwrap();
        fs::rename(root.join("usr"), ambient.join("usr")).unwrap();
        create_marked_tree(&root.join("usr"));
    });
    arm_retained_exchange_syscall_fault(RetainedExchangeSyscallFault::SuccessWithoutApply);

    let result =
        parents
            .attempt_usr_exchange_once()
            .reconcile(&fixture.installation, &fixture.record, baseline, projection);

    assert_eq!(retained_exchange_syscall_count(), 1);
    expect_ambiguous(result);
}

#[test]
fn reverse_exchange_reconciliation_rejects_non_post_baseline_after_fresh_capture() {
    let fixture = ReconciliationFixture::post();
    exchange_usr_layout(&fixture.installation.root);
    let wrong_baseline = capture_snapshot(&fixture.installation, &fixture.record).unwrap();
    let wrong_projection = ProjectedReverseNamespace::capture(&wrong_baseline, &fixture.record).unwrap();
    assert_eq!(wrong_projection.layout(), UsrExchangeLayout::Pre);
    exchange_usr_layout(&fixture.installation.root);
    let current = capture_snapshot(&fixture.installation, &fixture.record).unwrap();
    let parents = RetainedReverseExchangeParents::capture(&current, &fixture.record).unwrap();
    let hook_runs = Arc::new(AtomicUsize::new(0));
    let hook_observer = Arc::clone(&hook_runs);
    arm_before_reverse_exchange_reconciliation_capture(move || {
        hook_observer.fetch_add(1, Ordering::SeqCst);
    });
    reset_retained_exchange_syscall_count();

    let result = parents.attempt_usr_exchange_once().reconcile(
        &fixture.installation,
        &fixture.record,
        wrong_baseline,
        wrong_projection,
    );

    assert_eq!(retained_exchange_syscall_count(), 1);
    assert_eq!(hook_runs.load(Ordering::SeqCst), 1);
    expect_ambiguous(result);
}

#[test]
fn reverse_exchange_reconciliation_rejects_projection_from_another_post_snapshot() {
    let fixture = ReconciliationFixture::post();
    let ambient = fixture
        .installation
        .state_quarantine_dir()
        .join("ambient-mismatched-projection");
    fs::create_dir(&ambient).unwrap();
    fs::set_permissions(&ambient, fs::Permissions::from_mode(0o700)).unwrap();
    let other_post = capture_snapshot(&fixture.installation, &fixture.record).unwrap();
    let mismatched_projection = ProjectedReverseNamespace::capture(&other_post, &fixture.record).unwrap();
    assert_eq!(mismatched_projection.layout(), UsrExchangeLayout::Post);
    fs::remove_dir(&ambient).unwrap();
    let baseline = capture_snapshot(&fixture.installation, &fixture.record).unwrap();
    let baseline_projection = ProjectedReverseNamespace::capture(&baseline, &fixture.record).unwrap();
    assert_ne!(baseline_projection, mismatched_projection);
    let parents = RetainedReverseExchangeParents::capture(&baseline, &fixture.record).unwrap();
    let hook_runs = Arc::new(AtomicUsize::new(0));
    let hook_observer = Arc::clone(&hook_runs);
    arm_before_reverse_exchange_reconciliation_capture(move || {
        hook_observer.fetch_add(1, Ordering::SeqCst);
    });
    arm_retained_exchange_syscall_fault(RetainedExchangeSyscallFault::SuccessWithoutApply);

    let result = parents.attempt_usr_exchange_once().reconcile(
        &fixture.installation,
        &fixture.record,
        baseline,
        mismatched_projection,
    );

    assert_eq!(retained_exchange_syscall_count(), 1);
    assert_eq!(hook_runs.load(Ordering::SeqCst), 1);
    expect_ambiguous(result);
}

fn exchange_usr_layout(root: &Path) {
    let live = root.join("usr");
    let staged = root.join(".cast/root/staging/usr");
    let parked: PathBuf = root.join(".cast/root/.reverse-reconciliation-test-parked");
    fs::rename(&live, &parked).unwrap();
    fs::rename(&staged, &live).unwrap();
    fs::rename(&parked, &staged).unwrap();
}
