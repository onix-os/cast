#[test]
fn immutable_package_paths_accept_materializable_rule_kinds() {
    let mut spec = package();
    spec.outputs[0].paths = vec![
        PathSpec::Any { path: "*".to_owned() },
        PathSpec::Exe {
            path: "/usr/bin/*".to_owned(),
        },
        PathSpec::Symlink {
            path: "/usr/lib/*.so".to_owned(),
        },
    ];

    spec.validate().unwrap();
}

#[test]
fn package_v3_reserved_special_path_rule_is_rejected_with_its_exact_field() {
    let mut spec = package();
    spec.outputs[0].paths = vec![PathSpec::Special {
        path: "/usr/lib/example/events.fifo".to_owned(),
    }];

    let error = spec.validate().unwrap_err();
    assert!(matches!(
        error,
        PackageConversionError::UnsupportedSpecialPathRule { ref field }
            if field == "outputs[0].paths[0]"
    ));
    assert_eq!(error.field(), "outputs[0].paths[0]");
    assert!(error.to_string().contains("reserved by package-v3"));
    assert!(error.to_string().contains("immutable package layouts"));
}
