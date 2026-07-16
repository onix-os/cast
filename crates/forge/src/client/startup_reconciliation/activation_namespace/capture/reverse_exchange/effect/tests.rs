use std::{fs, path::PathBuf};

use crate::{
    Installation,
    test_support::private_installation_tempdir,
    transition_identity::{
        RetainedExchangeSyscallFault, arm_retained_exchange_syscall_fault, reset_retained_exchange_syscall_count,
        retained_exchange_syscall_count,
    },
};

use super::*;
use crate::client::startup_reconciliation::activation_namespace::capture::{
    Budget, ReverseExchangeParentIdentity, controlled_directory_witness, open_directory,
};

const LIVE_CONTENT: &str = "live-candidate";
const STAGED_CONTENT: &str = "staged-previous";

struct EffectFixture {
    _temporary: tempfile::TempDir,
    live_identity: PathBuf,
    staged_identity: PathBuf,
    parents: RetainedReverseExchangeParents,
}

impl EffectFixture {
    fn new() -> Self {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let root_path = installation.root.clone();
        let roots_path = root_path.join(".cast/root");
        let staging_path = roots_path.join("staging");
        let live_identity = root_path.join("usr/identity");
        let staged_identity = staging_path.join("usr/identity");
        fs::create_dir(root_path.join("usr")).unwrap();
        fs::create_dir(staging_path.join("usr")).unwrap();
        fs::write(&live_identity, LIVE_CONTENT).unwrap();
        fs::write(&staged_identity, STAGED_CONTENT).unwrap();

        let mut budget = Budget::new().unwrap();
        let root = open_directory(installation.root_directory(), c".", &root_path, &mut budget).unwrap();
        let roots = open_directory(&root, c".cast/root", &roots_path, &mut budget).unwrap();
        let staging = open_directory(&roots, c"staging", &staging_path, &mut budget).unwrap();
        let root_witness = controlled_directory_witness(&root, &root_path).unwrap();
        let roots_witness = controlled_directory_witness(&roots, &roots_path).unwrap();
        let staging_witness = controlled_directory_witness(&staging, &staging_path).unwrap();
        let parents = RetainedReverseExchangeParents {
            root,
            roots,
            staging,
            root_path,
            roots_path,
            staging_path,
            roots_witness,
            identity: ReverseExchangeParentIdentity::from_witnesses(root_witness, staging_witness).unwrap(),
        };

        Self {
            _temporary: temporary,
            live_identity,
            staged_identity,
            parents,
        }
    }
}

fn assert_layout(live_identity: &PathBuf, staged_identity: &PathBuf, live: &str, staged: &str) {
    assert_eq!(fs::read_to_string(live_identity).unwrap(), live);
    assert_eq!(fs::read_to_string(staged_identity).unwrap(), staged);
}

#[test]
fn reverse_exchange_effect_retains_normal_success_after_single_applied_attempt() {
    let EffectFixture {
        _temporary,
        live_identity,
        staged_identity,
        parents,
    } = EffectFixture::new();
    reset_retained_exchange_syscall_count();

    let pending = parents.attempt_usr_exchange_once();

    assert_eq!(retained_exchange_syscall_count(), 1);
    assert!(pending.raw_report.is_ok());
    assert_layout(&live_identity, &staged_identity, STAGED_CONTENT, LIVE_CONTENT);
}

#[test]
fn reverse_exchange_effect_retains_error_after_single_applied_attempt() {
    let EffectFixture {
        _temporary,
        live_identity,
        staged_identity,
        parents,
    } = EffectFixture::new();
    arm_retained_exchange_syscall_fault(RetainedExchangeSyscallFault::ErrorAfterApply);

    let pending = parents.attempt_usr_exchange_once();

    assert_eq!(retained_exchange_syscall_count(), 1);
    assert!(pending.raw_report.is_err());
    assert!(pending.parents.root.metadata().is_ok());
    assert_layout(&live_identity, &staged_identity, STAGED_CONTENT, LIVE_CONTENT);
}

#[test]
fn reverse_exchange_effect_retains_success_after_single_unapplied_attempt() {
    let EffectFixture {
        _temporary,
        live_identity,
        staged_identity,
        parents,
    } = EffectFixture::new();
    arm_retained_exchange_syscall_fault(RetainedExchangeSyscallFault::SuccessWithoutApply);

    let pending = parents.attempt_usr_exchange_once();

    assert_eq!(retained_exchange_syscall_count(), 1);
    assert!(pending.raw_report.is_ok());
    assert!(pending.parents.staging.metadata().is_ok());
    assert_layout(&live_identity, &staged_identity, LIVE_CONTENT, STAGED_CONTENT);
}

#[test]
fn reverse_exchange_effect_never_retries_reported_error_without_application() {
    let EffectFixture {
        _temporary,
        live_identity,
        staged_identity,
        parents,
    } = EffectFixture::new();
    arm_retained_exchange_syscall_fault(RetainedExchangeSyscallFault::ErrorWithoutApply);

    let pending = parents.attempt_usr_exchange_once();

    assert_eq!(retained_exchange_syscall_count(), 1);
    assert!(pending.raw_report.is_err());
    assert_layout(&live_identity, &staged_identity, LIVE_CONTENT, STAGED_CONTENT);
}
