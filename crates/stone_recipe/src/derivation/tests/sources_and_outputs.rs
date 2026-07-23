#[test]
fn frozen_sources_apply_the_shared_secure_transport_policy() {
    let archive_cases = [
        "http://example.invalid/hello.tar.zst",
        "file:///tmp/hello.tar.zst",
        "ssh://example.invalid/hello.tar.zst",
    ];
    for value in archive_cases {
        let mut plan = sample_plan();
        let LockedSource::Archive { url, .. } = &mut plan.sources[0] else {
            unreachable!()
        };
        *url = value.to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::InvalidSourceUrl {
                index: 0,
                source: SourceUrlValidationError::UnsupportedScheme { .. },
            })
        ));
    }

    for value in ["https://example.invalid/source.git", "ssh://example.invalid/source.git"] {
        let mut plan = sample_plan();
        plan.sources = vec![sample_git_source(0, "hello.git")];
        let LockedSource::Git { url, .. } = &mut plan.sources[0] else {
            unreachable!()
        };
        *url = value.to_owned();
        plan.validate().unwrap();
    }

    for value in [
        "http://example.invalid/source.git",
        "git://example.invalid/source.git",
        "file:///tmp/source.git",
    ] {
        let mut plan = sample_plan();
        plan.sources = vec![sample_git_source(0, "hello.git")];
        let LockedSource::Git { url, .. } = &mut plan.sources[0] else {
            unreachable!()
        };
        *url = value.to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::InvalidSourceUrl {
                index: 0,
                source: SourceUrlValidationError::UnsupportedScheme { .. },
            })
        ));
    }
}

#[test]
fn frozen_source_url_errors_are_field_specific_and_secret_free() {
    for value in [
        "https://user:do-not-print@example.invalid/hello.tar.zst",
        "https://example.invalid/hello.tar.zst#do-not-print",
    ] {
        let mut plan = sample_plan();
        let LockedSource::Archive { url, .. } = &mut plan.sources[0] else {
            unreachable!()
        };
        *url = value.to_owned();
        let error = plan.validate().unwrap_err();
        let message = error.to_string();
        assert!(message.starts_with("sources[0].url:"));
        assert!(!message.contains("user"));
        assert!(!message.contains("do-not-print"));
    }
}

#[test]
fn validation_requires_a_canonical_lowercase_git_commit() {
    for value in [
        String::new(),
        "a".repeat(39),
        "a".repeat(41),
        format!("{}g", "a".repeat(39)),
        "A".repeat(40),
        "é".repeat(20),
    ] {
        let mut plan = sample_plan();
        plan.sources = vec![sample_git_source(0, "hello.git")];
        let LockedSource::Git { commit, .. } = &mut plan.sources[0] else {
            unreachable!()
        };
        *commit = value.clone();

        let error = plan.validate().unwrap_err();
        assert!(matches!(
            error,
            DerivationValidationError::InvalidGitCommit {
                index: 0,
                value: ref found,
            } if found == &value
        ));
        assert_eq!(
            error.to_string(),
            format!(
                "sources[0].commit: expected exactly 40 lowercase ASCII hexadecimal characters, found `{value}`"
            )
        );
    }
}

#[test]
fn validation_requires_a_canonical_archive_sha256() {
    for value in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        format!("{}g", "a".repeat(63)),
        "A".repeat(64),
    ] {
        let mut plan = sample_plan();
        let LockedSource::Archive { sha256, .. } = &mut plan.sources[0] else {
            unreachable!()
        };
        *sha256 = value;
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::InvalidArchiveSha256 { index: 0, .. })
        ));
    }
}

#[test]
fn validation_requires_a_lowercase_git_materialization_sha256() {
    for value in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        format!("{}g", "a".repeat(63)),
        "A".repeat(64),
        "é".repeat(32),
    ] {
        let mut plan = sample_plan();
        plan.sources = vec![sample_git_source(0, "hello.git")];
        let LockedSource::Git {
            materialization_sha256, ..
        } = &mut plan.sources[0]
        else {
            unreachable!()
        };
        *materialization_sha256 = value.clone();

        let error = plan.validate().unwrap_err();
        assert!(matches!(
            error,
            DerivationValidationError::InvalidGitMaterializationSha256 {
                index: 0,
                value: ref found,
            } if found == &value
        ));
        assert_eq!(
            error.to_string(),
            format!(
                "sources[0].materialization_sha256: expected exactly 64 lowercase ASCII hexadecimal characters, found `{value}`"
            )
        );
    }
}

#[test]
fn validation_rejects_duplicate_source_materialization_destinations_across_kinds() {
    let mut plan = sample_plan();
    plan.sources.push(LockedSource::Git {
        order: 1,
        url: "https://example.invalid/other.git".to_owned(),
        requested_ref: "main".to_owned(),
        commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        materialization_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
        directory: "hello.tar.zst".to_owned(),
    });

    let error = plan.validate().unwrap_err();
    assert!(matches!(
        error,
        DerivationValidationError::DuplicateSourceDestination {
            index: 1,
            field: "directory",
            first_index: 0,
            first_field: "filename",
            ref value,
        } if value == "hello.tar.zst"
    ));
    assert_eq!(
        error.to_string(),
        "sources[1].directory: duplicate materialization destination \"hello.tar.zst\"; first declared at sources[0].filename"
    );
}

#[test]
fn validation_rejects_planned_output_cycles_with_the_closing_edge() {
    let mut plan = sample_plan();
    plan.outputs[0].runtime_inputs.push(OutputRelation::Planned {
        output: "dev".to_owned(),
    });
    plan.outputs.push(OutputPlan {
        name: "dev".to_owned(),
        package_name: "hello-devel".to_owned(),
        include_in_manifest: true,
        summary: None,
        description: None,
        provides_exclude: Vec::new(),
        runtime_exclude: Vec::new(),
        runtime_inputs: vec![OutputRelation::Planned {
            output: "out".to_owned(),
        }],
        conflicts: Vec::new(),
    });

    let error = plan.validate().unwrap_err();
    assert!(matches!(
        error,
        DerivationValidationError::PlannedOutputCycle { ref field, ref cycle }
            if field == "outputs[1].runtime_inputs[0]"
                && cycle.iter().map(String::as_str).eq(["out", "dev", "out"])
    ));
    assert_eq!(
        error.to_string(),
        "outputs[1].runtime_inputs[0]: planned output dependency cycle: out -> dev -> out"
    );
}

#[test]
fn changing_only_git_materialization_digest_changes_derivation_identity() {
    let mut first = sample_plan();
    first.sources = vec![sample_git_source(0, "hello.git")];
    first.validate().unwrap();

    let mut changed = first.clone();
    let LockedSource::Git {
        materialization_sha256, ..
    } = &mut changed.sources[0]
    else {
        unreachable!()
    };
    *materialization_sha256 = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_owned();
    changed.validate().unwrap();

    assert_ne!(first.canonical_bytes(), changed.canonical_bytes());
    assert_ne!(first.derivation_id(), changed.derivation_id());
}

#[test]
fn source_construction_order_has_a_stable_canonical_identity() {
    let mut canonical = sample_plan();
    canonical.sources.push(sample_git_source(1, "hello.git"));
    canonical.validate().unwrap();

    let mut constructed_in_reverse = canonical.clone();
    constructed_in_reverse.sources.reverse();

    assert_eq!(canonical.canonical_bytes(), constructed_in_reverse.canonical_bytes());
    assert_eq!(canonical.derivation_id(), constructed_in_reverse.derivation_id());
}

#[test]
fn git_materialization_directory_is_validated_and_hashed() {
    let mut first = sample_plan();
    first.sources = vec![LockedSource::Git {
        order: 0,
        url: "https://example.invalid/hello.git".to_owned(),
        requested_ref: "main".to_owned(),
        commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        materialization_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
        directory: "hello.git".to_owned(),
    }];
    first.validate().unwrap();

    let mut changed = first.clone();
    if let LockedSource::Git { directory, .. } = &mut changed.sources[0] {
        *directory = "other.git".to_owned();
    } else {
        unreachable!()
    }
    changed.validate().unwrap();
    assert_ne!(first.derivation_id(), changed.derivation_id());

    if let LockedSource::Git { directory, .. } = &mut changed.sources[0] {
        *directory = "../escape".to_owned();
    }
    assert!(matches!(
        changed.validate(),
        Err(DerivationValidationError::UnsafeSourceDestination {
            index: 0,
            field: "directory",
            ..
        })
    ));
}
