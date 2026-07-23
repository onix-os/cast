#[test]
fn every_authored_source_field_is_validated_before_lowering() {
    for hash in ["short".to_owned(), "A".repeat(64), format!("{}g", "a".repeat(63))] {
        let mut source = archive_source();
        let UpstreamSpec::Archive { hash: value, .. } = &mut source else {
            unreachable!()
        };
        *value = hash;
        let mut invalid = package();
        invalid.sources.push(source);

        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::InvalidSource {
                source: UpstreamValidationError::InvalidArchiveSha256 { .. },
                ..
            }
        ));
        assert_eq!(error.field(), "sources[0].hash");
        assert!(error.to_string().contains("64 lowercase ASCII hexadecimal"));
    }

    for rename in [
        "",
        ".",
        "..",
        "/escape",
        "nested/source",
        "nested\\source",
        "source\nname",
    ] {
        let mut source = archive_source();
        let UpstreamSpec::Archive { rename: value, .. } = &mut source else {
            unreachable!()
        };
        *value = Some(rename.to_owned());
        let mut invalid = package();
        invalid.sources.push(source);

        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::InvalidSource {
                source: UpstreamValidationError::InvalidMaterializationComponent { field: "rename", .. },
                ..
            }
        ));
        assert_eq!(error.field(), "sources[0].rename");
    }

    for unpack_dir in [
        "",
        ".",
        "..",
        "/escape",
        "nested//source",
        "nested/./source",
        "nested/../escape",
        "nested\\source",
        "source\nname",
    ] {
        let mut source = archive_source();
        let UpstreamSpec::Archive { unpack_dir: value, .. } = &mut source else {
            unreachable!()
        };
        *value = Some(unpack_dir.to_owned());
        let mut invalid = package();
        invalid.sources.push(source);

        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::InvalidSource {
                source: UpstreamValidationError::InvalidUnpackDir { .. },
                ..
            }
        ));
        assert_eq!(error.field(), "sources[0].unpack_dir");
        assert!(error.to_string().contains("normalized, non-empty relative path"));
    }

    for field in ["strip_dirs", "unpack_dir"] {
        let mut source = archive_source();
        let UpstreamSpec::Archive {
            strip_dirs,
            unpack,
            unpack_dir,
            ..
        } = &mut source
        else {
            unreachable!()
        };
        *unpack = false;
        if field == "strip_dirs" {
            *strip_dirs = Some(1);
        } else {
            *unpack_dir = Some("source".to_owned());
        }
        let mut invalid = package();
        invalid.sources.push(source);

        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::InvalidSource {
                source: UpstreamValidationError::OptionRequiresUnpack { .. },
                ..
            }
        ));
        assert_eq!(error.field(), format!("sources[0].{field}"));
        assert!(error.to_string().contains("unless `unpack` is true"));
    }

    for git_ref in ["", "main\nother"] {
        let mut source = git_source();
        let UpstreamSpec::Git { git_ref: value, .. } = &mut source else {
            unreachable!()
        };
        *value = git_ref.to_owned();
        let mut invalid = package();
        invalid.sources.push(source);

        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::InvalidSource {
                source: UpstreamValidationError::InvalidGitRef { .. },
                ..
            }
        ));
        assert_eq!(error.field(), "sources[0].git_ref");
    }

    let mut archive_without_name = archive_source();
    let UpstreamSpec::Archive { url, .. } = &mut archive_without_name else {
        unreachable!()
    };
    *url = "https://example.com/".to_owned();
    let mut git_without_name = git_source();
    let UpstreamSpec::Git { url, .. } = &mut git_without_name else {
        unreachable!()
    };
    *url = "https://example.com/".to_owned();
    for source in [archive_without_name, git_without_name] {
        let mut invalid = package();
        invalid.sources.push(source);

        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::InvalidSource {
                source: UpstreamValidationError::InvalidDefaultMaterializationName { .. },
                ..
            }
        ));
        assert_eq!(error.field(), "sources[0].url");
        assert!(error.to_string().contains("set `"));
    }
}

#[test]
fn source_validation_accepts_normalized_explicit_destinations() {
    let mut archive = archive_source();
    let UpstreamSpec::Archive {
        rename,
        strip_dirs,
        unpack_dir,
        ..
    } = &mut archive
    else {
        unreachable!()
    };
    *rename = Some("source archive;literal.tar.xz".to_owned());
    *strip_dirs = Some(0);
    *unpack_dir = Some("vendor/source tree".to_owned());

    let mut git = git_source();
    let UpstreamSpec::Git { git_ref, clone_dir, .. } = &mut git else {
        unreachable!()
    };
    *git_ref = "refs/tags/v1.0.0^{}".to_owned();
    *clone_dir = Some("git source".to_owned());

    let mut valid = package();
    valid.sources = vec![archive, git];

    valid.validate().unwrap();
}

#[test]
fn duplicate_source_materialization_destinations_are_rejected_before_resolution() {
    let mut archive = archive_source();
    let UpstreamSpec::Archive { rename, .. } = &mut archive else {
        unreachable!()
    };
    *rename = Some("same-source".to_owned());
    let mut git = git_source();
    let UpstreamSpec::Git { clone_dir, .. } = &mut git else {
        unreachable!()
    };
    *clone_dir = Some("same-source".to_owned());

    let mut invalid = package();
    invalid.sources = vec![archive, git];

    let error = invalid.validate().unwrap_err();
    assert!(matches!(
        error,
        PackageConversionError::DuplicateSourceMaterialization {
            ref field,
            ref first_field,
            ref value,
        } if field == "sources[1].clone_dir"
            && first_field == "sources[0].rename"
            && value == "same-source"
    ));
    assert_eq!(error.field(), "sources[1].clone_dir");
    assert_eq!(
        error.to_string(),
        "sources[1].clone_dir: materialization destination `same-source` duplicates `sources[0].rename`"
    );
}

#[test]
fn frozen_packages_require_network_content_to_be_locked_sources() {
    let mut invalid = package();
    invalid.options.networking = true;

    let error = invalid.validate().unwrap_err();

    assert!(matches!(
        error,
        PackageConversionError::FrozenBuildNetworkingUnsupported
    ));
    assert_eq!(error.field(), "options.networking");
    assert!(error.to_string().contains("locked sources"));
}
