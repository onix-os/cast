use std::{
    collections::BTreeMap,
    fs::Permissions,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    panic::{AssertUnwindSafe, catch_unwind},
    process::Command,
};

use fs_err as fs;

use super::*;

const TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn synthetic_bundle(fixture: &str) -> BTreeMap<String, Vec<u8>> {
    let mut bundle = BTreeMap::new();
    for index in 0..ledger::expected_stones(fixture) {
        let name = format!("cast-{fixture}-fixture-output-{index}.stone");
        let bytes = format!("Stone fixture={fixture} output={index}\n").into_bytes();
        assert!(bundle.insert(name, bytes).is_none());
    }
    assert!(
        bundle
            .insert(
                "manifest.x86_64.bin".to_owned(),
                format!("binary manifest {fixture}\n").into_bytes()
            )
            .is_none()
    );
    assert!(
        bundle
            .insert(
                "manifest.x86_64.jsonc".to_owned(),
                format!("{{\"fixture\":\"{fixture}\"}}\n").into_bytes(),
            )
            .is_none()
    );
    bundle
}

fn push_synthetic(builder: &mut ExecutionEvidenceBuilder, fixture: &'static str) {
    let plan = format!("canonical plan for {fixture}").into_bytes();
    let derivation_id = digest(&plan);
    let build_lock = format!("let fixture = \"{fixture}\"\n").into_bytes();
    let published = synthetic_bundle(fixture);
    let staged = published.clone();
    let preserved = published.clone();
    builder.push(FixtureEvidenceInputs {
        name: fixture,
        first_plan: &plan,
        first_derivation_id: &derivation_id,
        repeat_plan: &plan,
        repeat_derivation_id: &derivation_id,
        first_build_lock: &build_lock,
        first_build_lock_outcome: Some(WriteOutcome::Written),
        repeat_build_lock: &build_lock,
        repeat_build_lock_outcome: None,
        first_publication: Publication::Published,
        repeat_publication: Publication::Reused,
        published_after_first: &published,
        staged_after_repeat: &staged,
        published_after_repeat: &preserved,
    });
}

fn complete_evidence() -> ExecutionMatrixEvidence {
    let mut builder = ExecutionEvidenceBuilder::new(ExecutionFixtureSelection::All);
    for fixture in REQUIRED_EXECUTION_FIXTURES {
        push_synthetic(&mut builder, fixture);
    }
    let DelegatedExecutionOutcome::Completed(evidence) = builder.finish() else {
        panic!("complete synthetic matrix did not seal evidence");
    };
    evidence
}

#[test]
fn proof_v2_serializes_the_exact_complete_matrix_and_totals_within_its_bound() {
    let evidence = complete_evidence();
    let MatrixScope::Complete(totals) = &evidence.scope else {
        panic!("complete evidence has single-fixture scope");
    };
    let bytes = proof::serialize_for_test(&evidence.fixtures, totals, TEST_COMMIT);
    assert!(!bytes.is_empty());
    assert!(bytes.len() <= proof::MAX_PROOF_BYTES);
    assert_eq!(bytes.last(), Some(&b'\n'));

    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["schema"], "cast.fixtures-ci-proof.v2");
    assert_eq!(value["git_commit"], TEST_COMMIT);
    assert_eq!(value["bundle_ledger_schema"], "cast.fixtures-ci.bundle.v1");
    assert_eq!(value["totals"]["fixture_count"], 28);
    assert_eq!(value["totals"]["execution_count"], 56);
    assert_eq!(value["totals"]["bundle_validation_count"], 84);
    assert_eq!(value["totals"]["stone_count"], 134);
    assert_eq!(value["totals"]["manifest_count"], 56);
    assert_eq!(value["totals"]["artifact_count"], 190);
    assert!(value["totals"]["artifact_bytes"].as_u64().unwrap() > 0);
    let fixtures = value["fixtures"].as_array().unwrap();
    assert_eq!(fixtures.len(), 28);
    assert_eq!(fixtures[0]["name"], "autotools");
    assert_eq!(fixtures[8]["name"], "desktop-integration");
    assert_eq!(fixtures[9]["name"], "external-test-vectors");
    assert_eq!(fixtures[9]["artifacts"]["stone_count"], 2);
    assert_eq!(fixtures[9]["artifacts"]["artifact_count"], 4);
    assert_eq!(fixtures[11]["name"], "font-family");
    assert_eq!(fixtures[14]["name"], "gettext-localization");
    assert_eq!(fixtures[15]["name"], "go-module");
    assert_eq!(fixtures[16]["name"], "header-only-library");
    assert_eq!(fixtures[19]["name"], "multiple-sources");
    assert_eq!(fixtures[20]["name"], "pgo-workload");
    assert_eq!(fixtures[20]["artifacts"]["stone_count"], 1);
    assert_eq!(fixtures[20]["artifacts"]["artifact_count"], 3);
    assert_eq!(fixtures[22]["name"], "post-install-smoke-test");
    assert_eq!(fixtures[23]["name"], "python-module");
    assert_eq!(fixtures[24]["name"], "relation-policy");
    assert_eq!(fixtures[24]["artifacts"]["stone_count"], 1);
    assert_eq!(fixtures[24]["artifacts"]["artifact_count"], 3);
    assert_eq!(fixtures[26]["name"], "system-integration-assets");
    assert_eq!(fixtures[27]["name"], "userspace-profile");
    for fixture in fixtures {
        assert_eq!(fixture["plans"]["first"], fixture["plans"]["repeat"]);
        assert_eq!(
            fixture["plans"]["first"]["sha256"],
            fixture["plans"]["first"]["derivation_id"]
        );
        assert_eq!(fixture["build_locks"]["first"]["write_outcome"], "written");
        assert_eq!(fixture["build_locks"]["repeat"]["write_outcome"], "unchanged");
        assert_eq!(
            fixture["build_locks"]["first"]["sha256"],
            fixture["build_locks"]["repeat"]["sha256"]
        );
        assert_eq!(fixture["bundle_observations"].as_array().unwrap().len(), 3);
    }
}

#[test]
fn a_single_selection_seals_only_single_scope_and_never_claims_matrix_totals() {
    let mut builder = ExecutionEvidenceBuilder::new(ExecutionFixtureSelection::One("cmake"));
    push_synthetic(&mut builder, "cmake");
    let DelegatedExecutionOutcome::Completed(evidence) = builder.finish() else {
        panic!("single execution did not seal local evidence");
    };
    assert!(matches!(evidence.scope, MatrixScope::Single));
    assert_eq!(evidence.fixtures.len(), 1);
    assert_eq!(evidence.fixtures[0].name, "cmake");
}

#[test]
fn capability_loss_is_admitted_only_before_any_fixture_evidence() {
    let empty = ExecutionEvidenceBuilder::new(ExecutionFixtureSelection::All);
    assert!(matches!(
        empty.capability_unavailable(),
        DelegatedExecutionOutcome::CapabilityUnavailable
    ));

    let mut partial = ExecutionEvidenceBuilder::new(ExecutionFixtureSelection::All);
    push_synthetic(&mut partial, "autotools");
    assert!(catch_unwind(AssertUnwindSafe(|| partial.capability_unavailable())).is_err());
}

#[test]
fn builder_rejects_missing_surplus_and_out_of_order_fixture_evidence() {
    let missing = ExecutionEvidenceBuilder::new(ExecutionFixtureSelection::All);
    assert!(catch_unwind(AssertUnwindSafe(|| missing.finish())).is_err());

    let mut out_of_order = ExecutionEvidenceBuilder::new(ExecutionFixtureSelection::All);
    assert!(catch_unwind(AssertUnwindSafe(|| push_synthetic(&mut out_of_order, "cargo"))).is_err());

    let mut single = ExecutionEvidenceBuilder::new(ExecutionFixtureSelection::One("cmake"));
    push_synthetic(&mut single, "cmake");
    assert!(catch_unwind(AssertUnwindSafe(|| push_synthetic(&mut single, "cmake"))).is_err());
}

#[test]
fn plan_evidence_rejects_identity_drift_and_the_n_plus_one_byte_boundary() {
    let plan = b"canonical plan";
    let wrong = "00".repeat(32);
    assert!(catch_unwind(|| PlanObservation::capture(plan, &wrong)).is_err());

    let oversized = vec![b'x'; MAX_CANONICAL_PLAN_BYTES + 1];
    let oversized_id = digest(&oversized);
    assert!(catch_unwind(|| PlanObservation::capture(&oversized, &oversized_id)).is_err());
}

#[test]
fn artifact_and_bundle_byte_bounds_accept_n_and_reject_n_plus_one() {
    ledger::require_artifact_size_for_test(128 * 1024 * 1024);
    ledger::require_bundle_size_for_test(256 * 1024 * 1024);
    assert!(catch_unwind(|| ledger::require_artifact_size_for_test(128 * 1024 * 1024 + 1)).is_err());
    assert!(catch_unwind(|| ledger::require_bundle_size_for_test(0)).is_err());
    assert!(catch_unwind(|| ledger::require_bundle_size_for_test(256 * 1024 * 1024 + 1)).is_err());
}

#[test]
fn ledger_length_framing_and_raw_bytes_bind_names_sizes_and_contents() {
    let first = BTreeMap::from([("a.stone".to_owned(), b"bc".to_vec())]);
    let second = BTreeMap::from([("ab.stone".to_owned(), b"c".to_vec())]);
    let changed = BTreeMap::from([("a.stone".to_owned(), b"bd".to_vec())]);
    assert_eq!(
        ledger::ledger_digest(&first),
        "7163d5acedc73cb5c7a73a31f24a73925cddc9a6323f33c2ac3a6d235e4cb519",
        "Rust and POSIX fixture-ledger framing must share the frozen cross-language vector"
    );
    assert_ne!(ledger::ledger_digest(&first), ledger::ledger_digest(&second));
    assert_ne!(ledger::ledger_digest(&first), ledger::ledger_digest(&changed));
}

#[test]
fn fixture_sealing_rejects_plan_lock_and_each_bundle_observation_drift() {
    let fixture = "cmake";
    let plan = b"plan".to_vec();
    let derivation_id = digest(&plan);
    let first_lock = b"lock".to_vec();
    let changed_lock = b"changed lock".to_vec();
    let published = synthetic_bundle(fixture);
    let mut changed_bundle = published.clone();
    changed_bundle
        .get_mut("manifest.x86_64.bin")
        .unwrap()
        .extend_from_slice(b"changed");

    let lock_drift = || {
        FixtureEvidence::capture(FixtureEvidenceInputs {
            name: fixture,
            first_plan: &plan,
            first_derivation_id: &derivation_id,
            repeat_plan: &plan,
            repeat_derivation_id: &derivation_id,
            first_build_lock: &first_lock,
            first_build_lock_outcome: Some(WriteOutcome::Written),
            repeat_build_lock: &changed_lock,
            repeat_build_lock_outcome: None,
            first_publication: Publication::Published,
            repeat_publication: Publication::Reused,
            published_after_first: &published,
            staged_after_repeat: &published,
            published_after_repeat: &published,
        })
    };
    assert!(catch_unwind(AssertUnwindSafe(lock_drift)).is_err());

    for (first_lock_outcome, repeat_lock_outcome, first_publication, repeat_publication) in [
        (None, None, Publication::Published, Publication::Reused),
        (
            Some(WriteOutcome::Written),
            Some(WriteOutcome::Unchanged),
            Publication::Published,
            Publication::Reused,
        ),
        (
            Some(WriteOutcome::Written),
            None,
            Publication::Reused,
            Publication::Reused,
        ),
        (
            Some(WriteOutcome::Written),
            None,
            Publication::Published,
            Publication::Published,
        ),
    ] {
        let outcome_drift = || {
            FixtureEvidence::capture(FixtureEvidenceInputs {
                name: fixture,
                first_plan: &plan,
                first_derivation_id: &derivation_id,
                repeat_plan: &plan,
                repeat_derivation_id: &derivation_id,
                first_build_lock: &first_lock,
                first_build_lock_outcome: first_lock_outcome,
                repeat_build_lock: &first_lock,
                repeat_build_lock_outcome: repeat_lock_outcome,
                first_publication,
                repeat_publication,
                published_after_first: &published,
                staged_after_repeat: &published,
                published_after_repeat: &published,
            })
        };
        assert!(catch_unwind(AssertUnwindSafe(outcome_drift)).is_err());
    }

    let changed_plan = b"changed plan".to_vec();
    let changed_plan_id = digest(&changed_plan);
    let plan_drift = || {
        FixtureEvidence::capture(FixtureEvidenceInputs {
            name: fixture,
            first_plan: &plan,
            first_derivation_id: &derivation_id,
            repeat_plan: &changed_plan,
            repeat_derivation_id: &changed_plan_id,
            first_build_lock: &first_lock,
            first_build_lock_outcome: Some(WriteOutcome::Written),
            repeat_build_lock: &first_lock,
            repeat_build_lock_outcome: None,
            first_publication: Publication::Published,
            repeat_publication: Publication::Reused,
            published_after_first: &published,
            staged_after_repeat: &published,
            published_after_repeat: &published,
        })
    };
    assert!(catch_unwind(AssertUnwindSafe(plan_drift)).is_err());

    for (staged, preserved) in [(&changed_bundle, &published), (&published, &changed_bundle)] {
        let bundle_drift = || {
            FixtureEvidence::capture(FixtureEvidenceInputs {
                name: fixture,
                first_plan: &plan,
                first_derivation_id: &derivation_id,
                repeat_plan: &plan,
                repeat_derivation_id: &derivation_id,
                first_build_lock: &first_lock,
                first_build_lock_outcome: Some(WriteOutcome::Written),
                repeat_build_lock: &first_lock,
                repeat_build_lock_outcome: None,
                first_publication: Publication::Published,
                repeat_publication: Publication::Reused,
                published_after_first: &published,
                staged_after_repeat: staged,
                published_after_repeat: preserved,
            })
        };
        assert!(catch_unwind(AssertUnwindSafe(bundle_drift)).is_err());
    }
}

#[test]
fn artifact_inventory_rejects_unsafe_unknown_and_wrong_cardinality_names() {
    for name in ["../escape.stone", "bad/name.stone", "manifest.x86_64.txt"] {
        let bundle = BTreeMap::from([(name.to_owned(), b"bytes".to_vec())]);
        assert!(catch_unwind(|| ledger::capture_inventory("generated-config", &bundle)).is_err());
    }
    let mut missing = synthetic_bundle("generated-config");
    missing.remove("manifest.x86_64.jsonc");
    assert!(catch_unwind(|| ledger::capture_inventory("generated-config", &missing)).is_err());
}

#[test]
fn proof_writer_publishes_one_bounded_synced_mode_644_json_file() {
    let evidence = complete_evidence();
    let MatrixScope::Complete(totals) = &evidence.scope else {
        panic!("complete evidence has single scope");
    };
    let root = crate::private_tempdir();
    let path = root.path().join("fixtures-ci-proof.json");
    proof::publish_to_for_test(&path, TEST_COMMIT, &evidence.fixtures, totals);

    let metadata = fs::symlink_metadata(&path).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o644);
    assert_eq!(metadata.nlink(), 1);
    assert!(usize::try_from(metadata.len()).unwrap() <= proof::MAX_PROOF_BYTES);
    assert!(!root.path().join(".fixtures-ci-proof.json.tmp").exists());
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(value["result"], "passed");
}

#[test]
#[ignore = "run by make fixture-proof-cross-boundary-test"]
fn rust_published_proof_passes_the_exact_shell_validator() {
    let evidence = complete_evidence();
    let MatrixScope::Complete(totals) = &evidence.scope else {
        panic!("complete evidence has single scope");
    };
    let root = crate::private_tempdir();
    let path = root.path().join("fixtures-ci-proof.json");
    proof::publish_to_for_test(&path, TEST_COMMIT, &evidence.fixtures, totals);

    let validator = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("misc/scripts/validate-fixtures-ci-proof.sh");
    let output = Command::new("timeout")
        .args(["--kill-after=1s", "30s"])
        .arg(&validator)
        .arg(&path)
        .arg(TEST_COMMIT)
        .output()
        .unwrap_or_else(|error| panic!("run exact fixture-proof validator {validator:?}: {error}"));
    assert!(
        output.status.success(),
        "Rust-published proof failed the exact shell validator with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn proof_publication_never_replaces_a_preexisting_or_racing_target() {
    let evidence = complete_evidence();
    let MatrixScope::Complete(totals) = &evidence.scope else {
        panic!("complete evidence has single scope");
    };
    let root = crate::private_tempdir();
    let path = root.path().join("fixtures-ci-proof.json");
    fs::write(&path, b"foreign proof").unwrap();
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            proof::publish_to_for_test(&path, TEST_COMMIT, &evidence.fixtures, totals)
        }))
        .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), b"foreign proof");

    let staged = root.path().join("staged");
    let raced = root.path().join("raced");
    fs::write(&staged, b"staged proof").unwrap();
    fs::write(&raced, b"racing proof").unwrap();
    let error = proof::rename_noreplace_for_test(&staged, &raced).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&staged).unwrap(), b"staged proof");
    assert_eq!(fs::read(&raced).unwrap(), b"racing proof");
}

#[test]
fn proof_writer_rejects_wrong_parent_mode_preexisting_temporary_and_invalid_commit() {
    let evidence = complete_evidence();
    let MatrixScope::Complete(totals) = &evidence.scope else {
        panic!("complete evidence has single scope");
    };
    let root = crate::private_tempdir();
    let path = root.path().join("fixtures-ci-proof.json");

    fs::set_permissions(root.path(), Permissions::from_mode(0o755)).unwrap();
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            proof::publish_to_for_test(&path, TEST_COMMIT, &evidence.fixtures, totals)
        }))
        .is_err()
    );
    assert!(!path.exists());
    fs::set_permissions(root.path(), Permissions::from_mode(0o700)).unwrap();

    let temporary = root.path().join(".fixtures-ci-proof.json.tmp");
    fs::write(&temporary, b"foreign temporary").unwrap();
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            proof::publish_to_for_test(&path, TEST_COMMIT, &evidence.fixtures, totals)
        }))
        .is_err()
    );
    assert_eq!(fs::read(&temporary).unwrap(), b"foreign temporary");
    assert!(!path.exists());
    fs::remove_file(&temporary).unwrap();

    for commit in [
        "abc",
        "A123456789abcdef0123456789abcdef01234567",
        "g123456789abcdef0123456789abcdef01234567",
    ] {
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                proof::publish_to_for_test(&path, commit, &evidence.fixtures, totals)
            }))
            .is_err()
        );
        assert!(!path.exists());
    }
}
