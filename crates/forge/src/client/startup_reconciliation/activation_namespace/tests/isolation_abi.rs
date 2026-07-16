use super::*;

// `client::root_abi` publishes through this fixed order. Keeping the consumer
// test explicit makes every crash prefix part of the startup policy contract.
const TRANSACTION_ISOLATION_PUBLICATION_ORDER: [(&str, &str); 5] = [
    ("sbin", "usr/sbin"),
    ("bin", "usr/bin"),
    ("lib", "usr/lib"),
    ("lib64", "usr/lib"),
    ("lib32", "usr/lib32"),
];
const ISOLATION_SCAFFOLD_ORDER: [&str; 6] = ["etc", "usr", "proc", "tmp", "dev", "sys"];

#[test]
fn startup_activation_isolation_abi_crash_prefixes_match_trigger_phase_contract() {
    for published in 0..=TRANSACTION_ISOLATION_PUBLICATION_ORDER.len() {
        let mut fixture = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
        fixture.record.candidate.id = Some(42);
        write_state_id(&fixture.installation.staging_path("usr"), b"42");

        for (name, target) in TRANSACTION_ISOLATION_PUBLICATION_ORDER.iter().copied().take(published) {
            symlink(target, fixture.installation.isolation_path(name)).unwrap();
        }

        fixture.record.phase = Phase::CandidatePrepared;
        assert_eq!(
            fixture.assess(),
            Ok(()),
            "CandidatePrepared must reopen after exact isolation prefix {published}"
        );

        for phase in [Phase::TransactionTriggersStarted, Phase::TransactionTriggersComplete] {
            fixture.record.phase = phase;
            let expected = if published == TRANSACTION_ISOLATION_PUBLICATION_ORDER.len() {
                Ok(())
            } else {
                Err(NamespacePolicyConflict::IsolationAbiIncomplete)
            };
            assert_eq!(
                fixture.assess(),
                expected,
                "startup policy mismatch for {phase:?} after exact isolation prefix {published}"
            );
        }
    }

    for retained in 0..=ISOLATION_SCAFFOLD_ORDER.len() {
        let mut fixture = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
        fixture.record.candidate.id = Some(42);
        write_state_id(&fixture.installation.staging_path("usr"), b"42");
        for (name, target) in TRANSACTION_ISOLATION_PUBLICATION_ORDER {
            symlink(target, fixture.installation.isolation_path(name)).unwrap();
        }
        for name in ISOLATION_SCAFFOLD_ORDER.iter().take(retained) {
            create_private_directory(&fixture.installation.isolation_path(name));
        }

        for phase in [
            Phase::CandidatePrepared,
            Phase::TransactionTriggersStarted,
            Phase::TransactionTriggersComplete,
        ] {
            fixture.record.phase = phase;
            assert_eq!(
                fixture.assess(),
                Ok(()),
                "startup must retain controlled empty scaffold prefix {retained} at {phase:?}"
            );
        }
    }

    let mut populated = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    populated.record.candidate.id = Some(42);
    populated.record.phase = Phase::TransactionTriggersStarted;
    write_state_id(&populated.installation.staging_path("usr"), b"42");
    for (name, target) in TRANSACTION_ISOLATION_PUBLICATION_ORDER {
        symlink(target, populated.installation.isolation_path(name)).unwrap();
    }
    let etc = populated.installation.isolation_path("etc");
    create_private_directory(&etc);
    fs::write(etc.join("foreign"), b"foreign").unwrap();
    assert!(matches!(
        populated.snapshot(),
        Err(CaptureError::UnexpectedWrapperEntry { wrapper, name })
            if wrapper == etc && name == b"foreign"
    ));
}
