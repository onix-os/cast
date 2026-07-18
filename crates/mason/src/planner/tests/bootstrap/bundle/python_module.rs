fn assert_python_module_fixture(planned: &super::super::Planned, packages: &BTreeMap<String, PackageImage>) {
    const FIXTURE: &str = "python-module";
    const TREE: &str = "cast-python-module-fixture-1.0.0";
    const PREFIX: &str = "lib/python3.14/site-packages";
    const MODULE: &str = "cast_python_module_fixture";
    const DIST: &str = "cast_python_module_fixture-1.0.0.dist-info";
    const EXECUTABLE: &str = "bin/cast-python-module-fixture";

    assert_eq!(
        packages.len(),
        1,
        "{FIXTURE}: emitted bundle must contain exactly one package"
    );
    let (output_name, root) = packages.first_key_value().unwrap();
    assert_eq!(output_name.as_str(), "cast-python-module-fixture");
    let [root_plan] = planned.plan.outputs.as_slice() else {
        panic!("{FIXTURE}: frozen plan must contain exactly one output");
    };
    assert_eq!(root_plan.name, "out");
    assert_eq!(root_plan.package_name, *output_name);
    assert!(root_plan.include_in_manifest);
    assert_eq!(
        root_plan
            .runtime_inputs
            .iter()
            .map(|relation| match relation {
                OutputRelation::Locked { relation, .. } => relation.canonical_name(),
                OutputRelation::Planned { output } => {
                    panic!("{FIXTURE}: runtime unexpectedly targets local output {output}")
                }
            })
            .collect::<Vec<_>>(),
        ["binary(python3)", "python(typing-extensions)"]
    );
    assert_eq!(
        root_plan.summary.as_deref(),
        Some("Pinned offline PEP 517 Python module fixture")
    );
    assert_eq!(
        root_plan.description.as_deref(),
        Some(
            "A reproducible pure-Python wheel built, tested, installed, and packaged from one exact self-authored source archive."
        )
    );

    let module = |name: &str| format!("{PREFIX}/{MODULE}/{name}");
    let dist = |name: &str| format!("{PREFIX}/{DIST}/{name}");
    let init = module("__init__.py");
    let main = module("__main__.py");
    let codec = module("codec.py");
    let metadata = dist("METADATA");
    let record = dist("RECORD");
    let wheel = dist("WHEEL");
    let entry_points = dist("entry_points.txt");
    let license = dist("licenses/LICENSE");
    let top_level = dist("top_level.txt");
    assert_leaf_paths(
        FIXTURE,
        "out",
        root,
        [
            EXECUTABLE,
            init.as_str(),
            main.as_str(),
            codec.as_str(),
            license.as_str(),
            metadata.as_str(),
            record.as_str(),
            wheel.as_str(),
            entry_points.as_str(),
            top_level.as_str(),
        ],
    );
    assert_no_directories(FIXTURE, "out", root);

    assert_regular(
        FIXTURE,
        root,
        &init,
        0o644,
        tracked_bytes(TREE, "src/cast_python_module_fixture/__init__.py"),
    );
    assert_regular(
        FIXTURE,
        root,
        &main,
        0o644,
        tracked_bytes(TREE, "src/cast_python_module_fixture/__main__.py"),
    );
    assert_regular(
        FIXTURE,
        root,
        &codec,
        0o644,
        tracked_bytes(TREE, "src/cast_python_module_fixture/codec.py"),
    );
    assert_regular(FIXTURE, root, &license, 0o644, tracked_bytes(TREE, "LICENSE"));

    assert_eq!(root.layouts[EXECUTABLE].mode & 0o777, 0o755);
    let executable = regular_bytes(FIXTURE, root, EXECUTABLE);
    assert!(executable.starts_with(b"#!/usr/bin/python3\n"));
    assert!(!executable.starts_with(b"\x7fELF"));
    for marker in [
        b"cast_python_module_fixture.__main__".as_slice(),
        b"import main".as_slice(),
        b"sys.exit(main())".as_slice(),
    ] {
        assert!(
            executable.windows(marker.len()).any(|window| window == marker),
            "{FIXTURE}: installed console entry point lost marker {:?}",
            String::from_utf8_lossy(marker)
        );
    }

    let metadata_bytes = regular_bytes(FIXTURE, root, &metadata);
    let metadata_text = std::str::from_utf8(metadata_bytes).expect("Python METADATA must be UTF-8");
    for marker in [
        "Name: cast-python-module-fixture\n",
        "Version: 1.0.0\n",
        "Requires-Python: >=3.14\n",
        "Requires-Dist: typing-extensions>=4.15\n",
    ] {
        assert_eq!(
            metadata_text.matches(marker).count(),
            1,
            "{FIXTURE}: METADATA marker drifted: {marker:?}"
        );
    }
    let wheel_text = std::str::from_utf8(regular_bytes(FIXTURE, root, &wheel)).expect("WHEEL must be UTF-8");
    assert!(wheel_text.contains("Root-Is-Purelib: true\n"));
    assert!(wheel_text.contains("Tag: py3-none-any\n"));
    assert_eq!(
        regular_bytes(FIXTURE, root, &entry_points),
        b"[console_scripts]\ncast-python-module-fixture = cast_python_module_fixture.__main__:main\n"
    );
    assert_eq!(
        regular_bytes(FIXTURE, root, &top_level),
        b"cast_python_module_fixture\n"
    );
    let record_text =
        std::str::from_utf8(regular_bytes(FIXTURE, root, &record)).expect("installed RECORD must be UTF-8");
    let expected_record_paths = BTreeSet::from([
        "../../../bin/cast-python-module-fixture".to_owned(),
        format!("{MODULE}/__init__.py"),
        format!("{MODULE}/__main__.py"),
        format!("{MODULE}/codec.py"),
        format!("{DIST}/METADATA"),
        format!("{DIST}/RECORD"),
        format!("{DIST}/WHEEL"),
        format!("{DIST}/entry_points.txt"),
        format!("{DIST}/licenses/LICENSE"),
        format!("{DIST}/top_level.txt"),
    ]);
    let rows = record_text
        .lines()
        .map(|line| line.split(',').collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), expected_record_paths.len());
    assert!(rows.iter().all(|row| row.len() == 3));
    assert_eq!(
        rows.iter().map(|row| row[0].to_owned()).collect::<BTreeSet<_>>(),
        expected_record_paths
    );
    for row in rows {
        if row[0] == format!("{DIST}/RECORD") {
            assert_eq!(&row[1..], ["", ""]);
        } else {
            assert!(row[1].starts_with("sha256="));
            assert!(row[2].parse::<u64>().is_ok_and(|size| size > 0));
        }
    }

    assert_exact_relations(
        FIXTURE,
        root,
        planned_output_dependencies(planned, root_plan),
        BTreeSet::from([root_plan.package_name.clone()]),
    );
}
