#[test]
fn direct_metadata_and_source_validation_keep_package_field_paths() {
    for name in [
        "",
        ".",
        "..",
        "/tmp/escape",
        "../../escape",
        "name/child",
        "name\\child",
    ] {
        let mut invalid = package();
        invalid.meta.pname = name.to_owned();
        let error = invalid.validate().unwrap_err();
        assert!(matches!(error, PackageConversionError::InvalidPackageName { .. }));
        assert_eq!(error.field(), "meta.pname");
    }

    let mut invalid = package();
    invalid.meta.version = "v1.0".to_owned();
    let error = invalid.validate().unwrap_err();
    assert!(matches!(
        error,
        PackageConversionError::VersionMustStartWithDigit { .. }
    ));
    assert_eq!(error.field(), "meta.version");

    for version in ["1/../../escape", "1\\escape", "1\ninvalid"] {
        let mut invalid = package();
        invalid.meta.version = version.to_owned();
        let error = invalid.validate().unwrap_err();
        assert!(matches!(error, PackageConversionError::InvalidVersionComponent { .. }));
        assert_eq!(error.field(), "meta.version");
    }

    let mut invalid = package();
    invalid.meta.release = 0;
    let error = invalid.validate().unwrap_err();
    assert!(matches!(
        error,
        PackageConversionError::ReleaseMustBePositive { release: 0 }
    ));
    assert_eq!(error.field(), "meta.release");

    let mut invalid_source = git_source();
    let UpstreamSpec::Git { url, .. } = &mut invalid_source else {
        unreachable!()
    };
    *url = "not a URL".to_owned();
    let mut invalid = package();
    invalid.sources.push(invalid_source);
    let error = invalid.validate().unwrap_err();
    assert!(matches!(
        error,
        PackageConversionError::InvalidSource {
            source: UpstreamValidationError::InvalidUrl { .. },
            ..
        }
    ));
    assert_eq!(error.field(), "sources[0].url");

    for clone_dir in ["", ".", "..", "nested/source", "nested\\source", "source\nname"] {
        let mut invalid = package();
        invalid.sources.push(UpstreamSpec::Git {
            url: "https://example.com/source.git".to_owned(),
            git_ref: "main".to_owned(),
            clone_dir: Some(clone_dir.to_owned()),
        });
        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::InvalidSource {
                source: UpstreamValidationError::InvalidMaterializationComponent { .. },
                ..
            }
        ));
        assert_eq!(error.field(), "sources[0].clone_dir");
    }

    let mut valid = package();
    valid.sources.push(UpstreamSpec::Git {
        url: "https://example.com/source.git".to_owned(),
        git_ref: "main".to_owned(),
        clone_dir: Some("custom-source.git".to_owned()),
    });
    valid.validate().unwrap();

    let mut invalid = package();
    invalid.sources.push(UpstreamSpec::Archive {
        url: "https://example.com/source.tar.xz".to_owned(),
        hash: "a".repeat(64),
        rename: None,
        strip_dirs: Some(256),
        unpack: true,
        unpack_dir: None,
    });
    let error = invalid.validate().unwrap_err();
    assert!(matches!(
        error,
        PackageConversionError::InvalidSource {
            source: UpstreamValidationError::InvalidStripDirs { .. },
            ..
        }
    ));
    assert_eq!(error.field(), "sources[0].strip_dirs");
}

#[test]
fn authored_sources_apply_the_shared_secure_transport_policy() {
    for value in [
        "http://example.com/source.tar.xz",
        "file:///tmp/source.tar.xz",
        "ssh://example.com/source.tar.xz",
    ] {
        let mut source = archive_source();
        let UpstreamSpec::Archive { url, .. } = &mut source else {
            unreachable!()
        };
        *url = value.to_owned();
        let mut invalid = package();
        invalid.sources.push(source);
        let error = invalid.validate().unwrap_err();
        assert_eq!(error.field(), "sources[0].url");
        assert!(matches!(
            error,
            PackageConversionError::InvalidSource {
                source: UpstreamValidationError::InvalidUrl {
                    source: crate::spec::SourceUrlValidationError::UnsupportedScheme { .. },
                },
                ..
            }
        ));
    }

    for value in ["https://example.com/source.git", "ssh://example.com/source.git"] {
        let mut source = git_source();
        let UpstreamSpec::Git { url, .. } = &mut source else {
            unreachable!()
        };
        *url = value.to_owned();
        let mut valid = package();
        valid.sources.push(source);
        valid.validate().unwrap();
    }

    for value in [
        "http://example.com/source.git",
        "git://example.com/source.git",
        "file:///tmp/source.git",
    ] {
        let mut source = git_source();
        let UpstreamSpec::Git { url, .. } = &mut source else {
            unreachable!()
        };
        *url = value.to_owned();
        let mut invalid = package();
        invalid.sources.push(source);
        let error = invalid.validate().unwrap_err();
        assert_eq!(error.field(), "sources[0].url");
        assert!(matches!(
            error,
            PackageConversionError::InvalidSource {
                source: UpstreamValidationError::InvalidUrl {
                    source: crate::spec::SourceUrlValidationError::UnsupportedScheme { .. },
                },
                ..
            }
        ));
    }
}

#[test]
fn authored_source_url_errors_do_not_echo_secrets() {
    for value in [
        "https://user:do-not-print@example.com/source.tar.xz",
        "https://example.com/source.tar.xz#do-not-print",
    ] {
        let mut source = archive_source();
        let UpstreamSpec::Archive { url, .. } = &mut source else {
            unreachable!()
        };
        *url = value.to_owned();
        let mut invalid = package();
        invalid.sources.push(source);
        let error = invalid.validate().unwrap_err();
        let message = error.to_string();
        assert!(message.starts_with("sources[0].url:"));
        assert!(!message.contains("user"));
        assert!(!message.contains("do-not-print"));
    }
}

#[test]
fn metadata_urls_and_license_expressions_fail_closed() {
    for homepage in ["not a URL", "ftp://example.com/package", "mailto:package@example.com"] {
        let mut invalid = package();
        invalid.meta.homepage = homepage.to_owned();
        let error = invalid.validate().unwrap_err();
        assert_eq!(error.field(), "meta.homepage");
        assert!(matches!(
            error,
            PackageConversionError::InvalidHomepage { .. } | PackageConversionError::UnsupportedHomepage { .. }
        ));
    }

    let mut credentials = package();
    credentials.meta.homepage = "https://user:secret@example.com/package".to_owned();
    let error = credentials.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::HomepageCredentials));
    assert_eq!(error.field(), "meta.homepage");

    let mut missing = package();
    missing.meta.license.clear();
    assert_eq!(missing.validate().unwrap_err().field(), "meta.license");

    for license in ["", " MPL-2.0", "MPL-2.0\n"] {
        let mut invalid = package();
        invalid.meta.license = vec![license.to_owned()];
        let error = invalid.validate().unwrap_err();
        assert!(matches!(error, PackageConversionError::InvalidText { .. }));
        assert_eq!(error.field(), "meta.license[0]");
    }

    let mut duplicate = package();
    duplicate.meta.license = vec!["MIT".to_owned(), "MIT".to_owned()];
    let error = duplicate.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::DuplicateValue { .. }));
    assert_eq!(error.field(), "meta.license[1]");
}

#[test]
fn architecture_and_tuning_selectors_are_portable_and_unique() {
    for architecture in ["", ".", "../x86_64", "x86_64\\escape", "x86_64\nother"] {
        let mut invalid = package();
        invalid.architectures = vec![architecture.to_owned()];
        let error = invalid.validate().unwrap_err();
        assert!(matches!(error, PackageConversionError::InvalidText { .. }));
        assert_eq!(error.field(), "architectures[0]");
    }

    let mut duplicate_architecture = package();
    duplicate_architecture.architectures = vec!["native".to_owned(), "native".to_owned()];
    assert!(matches!(
        duplicate_architecture.validate(),
        Err(PackageConversionError::DuplicateValue { .. })
    ));

    let tuning = |key: &str, value| NamedTuningSpec {
        key: key.to_owned(),
        value,
    };
    let mut duplicate_tuning = package();
    duplicate_tuning.tuning = vec![
        tuning("optimize", crate::TuningSpec::Enable),
        tuning("optimize", crate::TuningSpec::Disable),
    ];
    let error = duplicate_tuning.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::DuplicateValue { .. }));
    assert_eq!(error.field(), "tuning[1].key");

    let mut invalid_choice = package();
    invalid_choice.tuning = vec![tuning(
        "optimize",
        crate::TuningSpec::Config {
            value: "../speed".to_owned(),
        },
    )];
    assert_eq!(invalid_choice.validate().unwrap_err().field(), "tuning[0].value");
}

#[test]
fn output_patterns_are_compiled_and_deduplicated_at_the_package_boundary() {
    let mut invalid_regex = package();
    invalid_regex.outputs[0].runtime_exclude = vec!["[".to_owned()];
    let error = invalid_regex.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::InvalidRegex { .. }));
    assert_eq!(error.field(), "outputs[0].runtime_exclude[0]");

    let mut invalid_glob = package();
    invalid_glob.outputs[0].paths = vec![PathSpec::Any { path: "[".to_owned() }];
    let error = invalid_glob.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::InvalidGlob { .. }));
    assert_eq!(error.field(), "outputs[0].paths[0].path");

    let mut empty_glob = package();
    empty_glob.outputs[0].paths = vec![PathSpec::Any { path: String::new() }];
    assert!(matches!(
        empty_glob.validate(),
        Err(PackageConversionError::InvalidText { .. })
    ));

    let mut duplicate = package();
    duplicate.outputs[0].provides_exclude = vec!["^private$".to_owned(), "^private$".to_owned()];
    let error = duplicate.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::DuplicateValue { .. }));
    assert_eq!(error.field(), "outputs[0].provides_exclude[1]");

    let mut nul_summary = package();
    nul_summary.outputs[0].summary = Some("summary\0hidden".to_owned());
    assert_eq!(nul_summary.validate().unwrap_err().field(), "outputs[0].summary");
}
