const ISOLATION_ABI_LINKS: [(&str, &str); 5] = [
    ("sbin", "usr/sbin"),
    ("bin", "usr/bin"),
    ("lib", "usr/lib"),
    ("lib64", "usr/lib"),
    ("lib32", "usr/lib32"),
];
const TRANSACTION_ISOLATION_SCAFFOLDS: [&str; 5] = ["etc", "usr", "proc", "tmp", "dev"];

fn coordinator_at_transaction_isolation(
    candidate_kind: CandidateKind,
) -> (CoordinatorFixture, PreparedTransactionIsolationCoordinator) {
    let (fixture, coordinator) = coordinator_at_candidate_prepare_started(candidate_kind);
    let prepared = finish_candidate_prepare(coordinator).unwrap();
    let isolation = match prepared {
        PreparedStatefulTransitionCoordinator::NewStateIsolation(prepared) => prepared,
        PreparedStatefulTransitionCoordinator::ActiveReblitReservation(prepared) => prepared
            .reserve_for_transaction_triggers(&fixture.installation)
            .unwrap(),
        PreparedStatefulTransitionCoordinator::Archived(_) => {
            panic!("archived activation acquired transaction-isolation authority")
        }
    };
    assert_eq!(isolation.record().phase, Phase::CandidatePrepared);
    (fixture, isolation)
}

fn assert_isolation_abi(installation: &Installation) {
    for (name, target) in ISOLATION_ABI_LINKS {
        assert_eq!(
            fs::read_link(installation.isolation_path(name)).unwrap(),
            Path::new(target),
            "wrong retained isolation ABI link {name}",
        );
    }
}

fn replace_isolation_link(installation: &Installation, name: &str, target: &str) {
    let path = installation.isolation_path(name);
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(target, path).unwrap();
}

fn create_transaction_isolation_scaffolds(installation: &Installation) {
    for name in TRANSACTION_ISOLATION_SCAFFOLDS {
        let path = installation.isolation_path(name);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn journal_coordinator_transaction_isolation_foreign_entry_prevents_trigger_authority() {
    // A foreign occupant at one required ABI name is never replaced.
    let (fixture, isolation) = coordinator_at_transaction_isolation(CandidateKind::NewState);
    let prepared = isolation.record().clone();
    let foreign = fixture.installation.isolation_path("bin");
    write_canonical_file(&foreign, b"foreign isolation occupant");

    let failure = isolation
        .prepare_for_transaction_triggers(&fixture.installation)
        .unwrap_err();

    assert!(matches!(
        failure,
        TransactionIsolationAbiFailure::Publication {
            source: crate::client::Error::RootAbiLinkTypeConflict { path, .. },
            ..
        } if path == foreign
    ));
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign isolation occupant");
    assert!(!fixture.installation.isolation_path("sbin").exists());
    assert_eq!(reopen_record(&fixture.installation.root), prepared);

    // An unrelated extra name is equally incompatible with the exact scratch
    // root, even though it does not collide with any merged-/usr link.
    let (fixture, isolation) = coordinator_at_transaction_isolation(CandidateKind::NewState);
    let prepared = isolation.record().clone();
    let foreign = fixture.installation.isolation_path("foreign");
    write_canonical_file(&foreign, b"foreign isolation entry");

    let failure = isolation
        .prepare_for_transaction_triggers(&fixture.installation)
        .unwrap_err();

    assert!(matches!(
        failure,
        TransactionIsolationAbiFailure::Preflight {
            source: StatefulTransitionCoordinatorError::UnexpectedIsolationEntries { path, entries },
            ..
        } if path == fixture.installation.isolation_dir() && entries == ["foreign"]
    ));
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign isolation entry");
    assert_eq!(reopen_record(&fixture.installation.root), prepared);

    // The same foreign name appearing after ABI publication still blocks the
    // Started journal edge and the callback.
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let prepared = coordinator.record().clone();
    write_canonical_file(
        &fixture.installation.isolation_path("foreign"),
        b"late foreign isolation entry",
    );
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();

    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::Preflight {
            source: StatefulTransitionCoordinatorError::UnexpectedIsolationEntries { .. },
            ..
        }
    ));
    assert_eq!(calls.get(), 0);
    assert_eq!(reopen_record(&fixture.installation.root), prepared);

    // A callback which introduces an unauthorised persistent child cannot
    // publish Complete, even though the callback itself has run once.
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let installation = fixture.installation.clone();
    let calls = std::cell::Cell::new(0usize);
    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            write_canonical_file(
                &installation.isolation_path("foreign"),
                b"callback-created foreign isolation entry",
            );
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();

    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::PostEffectEvidence {
            source: StatefulTransitionCoordinatorError::UnexpectedIsolationEntries { .. },
            ..
        }
    ));
    assert_eq!(calls.get(), 1);
    assert_record_prefix(
        &reopen_record(&fixture.installation.root),
        Operation::NewState,
        Phase::TransactionTriggersStarted,
        6,
    );
}

#[test]
fn journal_coordinator_transaction_isolation_missing_or_substituted_before_started_runs_no_effect() {
    for substitute in [false, true] {
        let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
        let prepared = coordinator.record().clone();
        let bin = fixture.installation.isolation_path("bin");
        fs::remove_file(&bin).unwrap();
        if substitute {
            std::os::unix::fs::symlink("usr/bin", &bin).unwrap();
        }
        let calls = std::cell::Cell::new(0usize);

        let failure = coordinator
            .run_transaction_triggers(|_| {
                calls.set(calls.get() + 1);
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap_err();

        assert!(matches!(
            failure,
            StatefulTransactionTriggerFailure::Preflight {
                source: StatefulTransitionCoordinatorError::IsolationAbi(
                    crate::client::Error::RootAbiLinkMissing { .. }
                        | crate::client::Error::RootAbiLinkReplaced(_)
                ),
                ..
            }
        ));
        assert_eq!(calls.get(), 0);
        assert_eq!(reopen_record(&fixture.installation.root), prepared);
    }
}

#[test]
fn journal_coordinator_transaction_isolation_substitution_after_started_blocks_callback() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let installation = fixture.installation.clone();
    transaction_triggers::arm_before_transaction_trigger_effect_evidence(move || {
        replace_isolation_link(&installation, "bin", "usr/replaced-bin");
    });
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();

    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::PreEffectEvidence {
            source: StatefulTransitionCoordinatorError::IsolationAbi(_),
            ..
        }
    ));
    assert_eq!(calls.get(), 0);
    assert_record_prefix(
        &reopen_record(&fixture.installation.root),
        Operation::NewState,
        Phase::TransactionTriggersStarted,
        6,
    );
}

#[test]
fn journal_coordinator_transaction_isolation_is_mandatory_for_both_trigger_operations() {
    for (candidate_kind, operation) in [
        (CandidateKind::NewState, Operation::NewState),
        (CandidateKind::ActiveReblit, Operation::ActiveReblit),
    ] {
        let (fixture, coordinator) = coordinator_at_candidate_prepared(candidate_kind);
        assert_isolation_abi(&fixture.installation);
        let expected_path = fixture.installation.isolation_dir();
        let complete = coordinator
            .run_transaction_triggers(|authority| {
                let (installation, isolation) = authority.retained_isolation_root();
                assert_eq!(installation.isolation_dir(), expected_path);
                assert_eq!(isolation.path(), expected_path);
                isolation.revalidate().unwrap();
                create_transaction_isolation_scaffolds(installation);
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap();

        assert_eq!(complete.record().operation, operation);
        assert_eq!(complete.record().phase, Phase::TransactionTriggersComplete);
        assert_isolation_abi(&fixture.installation);
        complete.begin_usr_exchange_intent().unwrap();
    }
}

#[test]
fn journal_coordinator_transaction_isolation_tamper_blocks_later_readiness_boundary() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let complete = coordinator
        .run_transaction_triggers(|_| Ok::<(), TriggerEffectError>(()))
        .unwrap();
    replace_isolation_link(&fixture.installation, "lib64", "usr/replaced-lib");

    let failure = complete.begin_usr_exchange_intent().unwrap_err();

    assert!(matches!(
        failure,
        UsrExchangeIntentFailure::Preflight {
            source: StatefulTransitionCoordinatorError::IsolationAbi(_),
            ..
        }
    ));
    assert_record_prefix(
        &reopen_record(&fixture.installation.root),
        Operation::NewState,
        Phase::TransactionTriggersComplete,
        7,
    );
}

#[test]
fn journal_coordinator_transaction_isolation_candidate_prepared_and_started_are_reopenable() {
    let (fixture, isolation) = coordinator_at_transaction_isolation(CandidateKind::NewState);
    std::os::unix::fs::symlink("usr/sbin", fixture.installation.isolation_path("sbin")).unwrap();
    let sbin_identity = fs::symlink_metadata(fixture.installation.isolation_path("sbin"))
        .unwrap()
        .ino();
    let ready = isolation
        .prepare_for_transaction_triggers(&fixture.installation)
        .unwrap();
    assert_isolation_abi(&fixture.installation);
    assert_eq!(
        fs::symlink_metadata(fixture.installation.isolation_path("sbin"))
            .unwrap()
            .ino(),
        sbin_identity,
        "retry must retain an exact pre-existing isolation link",
    );
    let prepared = ready.record().clone();
    drop(ready);
    assert_eq!(reopen_record(&fixture.installation.root), prepared);

    let (fixture, ready) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let failure = ready
        .run_transaction_triggers(|_| Err(TriggerEffectError))
        .unwrap_err();
    assert!(matches!(failure, StatefulTransactionTriggerFailure::Effect { .. }));
    assert_isolation_abi(&fixture.installation);
    assert_record_prefix(
        &reopen_record(&fixture.installation.root),
        Operation::NewState,
        Phase::TransactionTriggersStarted,
        6,
    );
}
