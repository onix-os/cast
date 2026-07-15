fn assert_simple_fixture(fixture: &str, planned: &super::super::Planned, packages: &BTreeMap<String, PackageImage>) {
    for output in &planned.plan.outputs {
        assert_eq!(
            output.include_in_manifest,
            !matches!(output.name.as_str(), "dbginfo" | "32bit-dbginfo"),
            "{fixture}: default manifest membership drift for {}",
            output.name
        );
    }

    let (root_plan, root) = output(planned, packages, "out");
    if fixture == "custom" {
        let target = "share/cast-custom-fixture/payload.txt";
        assert_leaf_paths(fixture, "out", root, [target]);
        assert_no_directories(fixture, "out", root);
        assert_regular(
            fixture,
            root,
            target,
            0o644,
            tracked_bytes("cast-custom-fixture-1.0.0", "payload.txt"),
        );
        assert_exact_relations(
            fixture,
            root,
            planned_output_dependencies(planned, root_plan),
            BTreeSet::from([root_plan.package_name.clone()]),
        );
        for candidate in &planned.plan.outputs {
            if candidate.name != "out" {
                assert!(
                    packages[&candidate.package_name].layouts.is_empty(),
                    "{fixture}: unexpected path in default output {}",
                    candidate.name
                );
                assert_exact_relations(
                    fixture,
                    &packages[&candidate.package_name],
                    planned_output_dependencies(planned, candidate),
                    BTreeSet::from([candidate.package_name.clone()]),
                );
            }
        }
        return;
    }

    let (executable, messages) = match fixture {
        "cargo-vendored" => (
            "bin/cast-cargo-vendored-fixture".to_owned(),
            vec!["hello from ", "vendored Cargo fixture"],
        ),
        "hooks-patch" => ("bin/cast-hooks-fixture".to_owned(), vec!["pre_setup hook applied"]),
        _ => (
            format!("bin/cast-{fixture}-fixture"),
            vec![match fixture {
                "autotools" => "cast autotools fixture",
                "cargo" => "cast cargo fixture",
                "cmake" => "cast cmake fixture",
                "factory-override" => "Stone-native factory override: stone-override",
                "meson" => "cast meson fixture",
                other => panic!("{other}: simple fixture needs an explicit payload golden"),
            }],
        ),
    };
    assert_leaf_paths(fixture, "out", root, [executable.as_str()]);
    assert_no_directories(fixture, "out", root);
    let bytes = regular_bytes(fixture, root, &executable);
    assert_eq!(root.layouts[&executable].mode & 0o777, 0o755);
    let executable_elf = assert_runtime_elf(fixture, &executable, bytes, RuntimeElfKind::Executable);
    for message in messages {
        assert!(
            contains_bytes(bytes, message.as_bytes()),
            "{fixture}: installed executable does not contain tracked payload fragment {message:?}"
        );
    }
    let mut root_dependencies = planned_output_dependencies(planned, root_plan);
    root_dependencies.extend(executable_elf.dependencies.iter().cloned());
    assert_exact_relations(
        fixture,
        root,
        root_dependencies,
        BTreeSet::from([
            root_plan.package_name.clone(),
            format!(
                "binary({})",
                Path::new(&executable).file_name().unwrap().to_str().unwrap()
            ),
        ]),
    );

    let (debug_plan, debug) = output(planned, packages, "dbginfo");
    assert_debug_output(fixture, debug, &[executable_elf]);
    assert_exact_relations(
        fixture,
        debug,
        planned_output_dependencies(planned, debug_plan),
        BTreeSet::from([debug_plan.package_name.clone()]),
    );
    for candidate in &planned.plan.outputs {
        if !matches!(candidate.name.as_str(), "out" | "dbginfo") {
            assert!(
                packages[&candidate.package_name].layouts.is_empty(),
                "{fixture}: unexpected path in default output {}",
                candidate.name
            );
            assert_exact_relations(
                fixture,
                &packages[&candidate.package_name],
                planned_output_dependencies(planned, candidate),
                BTreeSet::from([candidate.package_name.clone()]),
            );
        }
    }
}

fn assert_daemon_fixture(planned: &super::super::Planned, packages: &BTreeMap<String, PackageImage>) {
    const FIXTURE: &str = "daemon-generated";
    let flags = planned
        .plan
        .outputs
        .iter()
        .map(|output| (output.name.as_str(), output.include_in_manifest))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        flags,
        BTreeMap::from([("out", true), ("docs", false), ("dbginfo", false)])
    );

    let (root_plan, root) = output(planned, packages, "out");
    let executable = "sbin/cast-daemon-fixture";
    let config = "share/defaults/cast-daemon-fixture/cast-daemon.conf";
    let service = "lib/systemd/system/cast-daemon.service";
    assert_leaf_paths(FIXTURE, "out", root, [executable, config, service]);
    assert_no_directories(FIXTURE, "out", root);
    assert_regular(
        FIXTURE,
        root,
        config,
        0o644,
        generated_daemon_bytes("packaging/cast-daemon.conf.in"),
    );
    assert_regular(
        FIXTURE,
        root,
        service,
        0o644,
        generated_daemon_bytes("packaging/cast-daemon.service.in"),
    );
    let executable_bytes = regular_bytes(FIXTURE, root, executable);
    assert_eq!(root.layouts[executable].mode & 0o777, 0o755);
    assert!(
        contains_bytes(executable_bytes, b"cast daemon fixture"),
        "{FIXTURE}: installed daemon omits its compiled identity"
    );
    assert!(
        contains_bytes(
            executable_bytes,
            b"/usr/share/defaults/cast-daemon-fixture/cast-daemon.conf"
        ),
        "{FIXTURE}: compiled daemon omits its configured default path"
    );
    let executable_elf = assert_runtime_elf(FIXTURE, executable, executable_bytes, RuntimeElfKind::Executable);
    let mut root_dependencies = planned_output_dependencies(planned, root_plan);
    root_dependencies.extend(executable_elf.dependencies.iter().cloned());
    assert_exact_relations(
        FIXTURE,
        root,
        root_dependencies,
        BTreeSet::from([
            root_plan.package_name.clone(),
            "sysbinary(cast-daemon-fixture)".to_owned(),
        ]),
    );

    let (docs_plan, docs) = output(planned, packages, "docs");
    let manual = "share/man/man8/cast-daemon.8";
    assert_leaf_paths(FIXTURE, "docs", docs, [manual]);
    assert_no_directories(FIXTURE, "docs", docs);
    assert_regular(
        FIXTURE,
        docs,
        manual,
        0o644,
        generated_daemon_bytes("packaging/cast-daemon.8.in"),
    );
    assert_exact_relations(
        FIXTURE,
        docs,
        planned_output_dependencies(planned, docs_plan),
        BTreeSet::from([docs_plan.package_name.clone()]),
    );

    let (debug_plan, debug) = output(planned, packages, "dbginfo");
    assert_debug_output(FIXTURE, debug, &[executable_elf]);
    assert_exact_relations(
        FIXTURE,
        debug,
        planned_output_dependencies(planned, debug_plan),
        BTreeSet::from([debug_plan.package_name.clone()]),
    );
}

fn generated_daemon_bytes(relative: &str) -> Vec<u8> {
    String::from_utf8(tracked_bytes("cast-daemon-fixture-1.0.0", relative))
        .expect("daemon fixture templates are UTF-8")
        .replace("@PROJECT_VERSION@", "1.0.0")
        .replace("@CMAKE_INSTALL_FULL_SBINDIR@", "/usr/sbin")
        .replace("@CMAKE_INSTALL_FULL_DATADIR@", "/usr/share")
        .into_bytes()
}

fn assert_split_fixture(planned: &super::super::Planned, packages: &BTreeMap<String, PackageImage>) {
    const FIXTURE: &str = "split";
    let flags = planned
        .plan
        .outputs
        .iter()
        .map(|output| (output.name.as_str(), output.include_in_manifest))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        flags,
        BTreeMap::from([
            ("out", true),
            ("libs", true),
            ("devel", true),
            ("docs", false),
            ("dbginfo", false)
        ])
    );
    assert!(
        planned
            .plan
            .collection_rules
            .iter()
            .any(|rule| { rule.output == "dbginfo" && rule.pattern == "/usr/lib/debug" }),
        "split: generated debug files have no explicit output rule"
    );

    let (root_plan, root) = output(planned, packages, "out");
    let (libs_plan, libs) = output(planned, packages, "libs");
    let (devel_plan, devel) = output(planned, packages, "devel");
    let (docs_plan, docs) = output(planned, packages, "docs");
    let (debug_plan, debug) = output(planned, packages, "dbginfo");
    assert_eq!(root_plan.package_name, "cast-split-fixture");
    assert_eq!(libs_plan.package_name, "cast-split-fixture-libs");
    assert_eq!(devel_plan.package_name, "cast-split-fixture-devel");
    assert_eq!(docs_plan.package_name, "cast-split-fixture-docs");
    assert_eq!(debug_plan.package_name, "cast-split-fixture-dbginfo");
    assert_eq!(
        planned
            .plan
            .outputs
            .iter()
            .map(|output| (output.name.as_str(), output.summary.as_deref()))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            ("out", Some("Split fixture executable and manual page")),
            ("libs", Some("Split fixture runtime library")),
            ("devel", Some("Split fixture development files")),
            ("docs", Some("Split fixture documentation")),
            ("dbginfo", Some("Split fixture debugging symbols")),
        ]),
        "split: output summaries are fixture metadata goldens"
    );
    assert!(
        planned.plan.outputs.iter().all(|output| output.description.is_none()),
        "split: fixture outputs must not grow implicit descriptions"
    );

    assert_leaf_paths(
        FIXTURE,
        "out",
        root,
        ["bin/cast-split-fixture", "share/man/man1/cast-split-fixture.1"],
    );
    assert_no_directories(FIXTURE, "out", root);
    assert_leaf_paths(
        FIXTURE,
        "libs",
        libs,
        ["lib/libcast-split.so.1", "lib/libcast-split.so.1.0.0"],
    );
    assert_no_directories(FIXTURE, "libs", libs);
    assert_leaf_paths(
        FIXTURE,
        "devel",
        devel,
        [
            "include/cast-split/libcastsplit.h",
            "lib/libcast-split.so",
            "lib/pkgconfig/cast-split.pc",
        ],
    );
    assert_no_directories(FIXTURE, "devel", devel);
    assert_leaf_paths(FIXTURE, "docs", docs, ["share/doc/cast-split-fixture/README.md"]);
    assert_no_directories(FIXTURE, "docs", docs);

    let executable = regular_bytes(FIXTURE, root, "bin/cast-split-fixture");
    assert_eq!(root.layouts["bin/cast-split-fixture"].mode & 0o777, 0o755);
    let executable_elf = assert_runtime_elf(
        FIXTURE,
        "bin/cast-split-fixture",
        executable,
        RuntimeElfKind::Executable,
    );
    assert_regular(
        FIXTURE,
        root,
        "share/man/man1/cast-split-fixture.1",
        0o644,
        tracked_bytes("cast-split-fixture-1.0.0", "cast-split-fixture.1"),
    );

    let library = regular_bytes(FIXTURE, libs, "lib/libcast-split.so.1.0.0");
    assert_eq!(libs.layouts["lib/libcast-split.so.1.0.0"].mode & 0o777, 0o755);
    let library_elf = assert_runtime_elf(
        FIXTURE,
        "lib/libcast-split.so.1.0.0",
        library,
        RuntimeElfKind::SharedLibrary,
    );
    assert!(
        contains_bytes(library, b"cast split fixture"),
        "split: shared library does not contain its tracked payload"
    );
    assert_symlink(FIXTURE, libs, "lib/libcast-split.so.1", "libcast-split.so.1.0.0");
    assert_symlink(FIXTURE, devel, "lib/libcast-split.so", "libcast-split.so.1");
    assert_regular(
        FIXTURE,
        devel,
        "include/cast-split/libcastsplit.h",
        0o644,
        tracked_bytes("cast-split-fixture-1.0.0", "libcastsplit.h"),
    );
    assert_regular(
        FIXTURE,
        docs,
        "share/doc/cast-split-fixture/README.md",
        0o644,
        tracked_bytes("cast-split-fixture-1.0.0", "README.md"),
    );
    assert_regular(
        FIXTURE,
        devel,
        "lib/pkgconfig/cast-split.pc",
        0o644,
        expected_split_pkgconfig(),
    );

    let mut root_dependencies = planned_output_dependencies(planned, root_plan);
    root_dependencies.extend(executable_elf.dependencies.iter().cloned());
    assert_exact_relations(
        FIXTURE,
        root,
        root_dependencies,
        BTreeSet::from([root_plan.package_name.clone(), "binary(cast-split-fixture)".to_owned()]),
    );
    let mut library_dependencies = planned_output_dependencies(planned, libs_plan);
    library_dependencies.extend(library_elf.dependencies.iter().cloned());
    let library_soname = library_elf
        .soname
        .as_deref()
        .expect("shared-library SONAME was structurally required");
    assert_exact_relations(
        FIXTURE,
        libs,
        library_dependencies,
        BTreeSet::from([
            libs_plan.package_name.clone(),
            format!("soname({library_soname}(x86_64))"),
        ]),
    );
    assert_exact_relations(
        FIXTURE,
        devel,
        planned_output_dependencies(planned, devel_plan),
        BTreeSet::from([devel_plan.package_name.clone(), "pkgconfig(cast-split)".to_owned()]),
    );
    for (plan, image) in [(docs_plan, docs), (debug_plan, debug)] {
        assert_exact_relations(
            FIXTURE,
            image,
            planned_output_dependencies(planned, plan),
            BTreeSet::from([plan.package_name.clone()]),
        );
    }
    assert_debug_output(FIXTURE, debug, &[executable_elf, library_elf]);

    let soname = "soname(libcast-split.so.1(x86_64))";
    assert!(
        root.meta
            .dependencies
            .iter()
            .map(Dependency::to_name)
            .any(|value| value == soname),
        "split: executable output does not depend on its shared-library SONAME"
    );
    assert!(
        libs.meta
            .providers
            .iter()
            .map(Provider::to_name)
            .any(|value| value == soname),
        "split: library output does not provide its SONAME"
    );
    assert!(
        devel
            .meta
            .providers
            .iter()
            .map(Provider::to_name)
            .any(|value| value == "pkgconfig(cast-split)"),
        "split: development output does not provide its pkg-config module"
    );
    assert!(
        devel
            .meta
            .dependencies
            .iter()
            .map(Dependency::to_name)
            .any(|value| value == libs_plan.package_name),
        "split: development package metadata omits the declared library output relation"
    );
    assert!(matches!(
        devel_plan.runtime_inputs.as_slice(),
        [OutputRelation::Planned { output }] if output == "libs"
    ));
}

fn assert_debug_output(fixture: &str, image: &PackageImage, originals: &[NativeElf]) {
    let expected_targets = originals
        .iter()
        .map(|original| debug_target(&original.build_id))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        image.layouts.keys().cloned().collect::<BTreeSet<_>>(),
        expected_targets,
        "{fixture}: debug output is not the exact build-ID closure of its native ELFs"
    );
    assert_eq!(
        expected_targets.len(),
        originals.len(),
        "{fixture}: distinct native ELFs unexpectedly share a build ID"
    );

    for original in originals {
        let target = debug_target(&original.build_id);
        let layout = &image.layouts[&target];
        assert_eq!(layout.mode & 0o777, 0o644);
        let debug_bytes = regular_bytes(fixture, image, &target);
        assert_debug_elf(fixture, &target, debug_bytes, original);
        let filename = Path::new(&target)
            .file_name()
            .and_then(|name| name.to_str())
            .expect("validated UTF-8 build-ID target");
        assert_eq!(
            original.debug_link.basename, filename,
            "{fixture}: runtime ELF .gnu_debuglink does not name its build-ID debug file"
        );
        assert_eq!(
            original.debug_link.crc32,
            gnu_debuglink_crc32(debug_bytes),
            "{fixture}: runtime ELF .gnu_debuglink CRC does not authenticate its debug file"
        );
    }
    assert_no_directories(fixture, "dbginfo", image);
}

fn assert_leaf_paths<'a>(
    fixture: &str,
    output: &str,
    image: &PackageImage,
    expected: impl IntoIterator<Item = &'a str>,
) {
    assert_eq!(
        image
            .layouts
            .iter()
            .filter(|(_, layout)| !matches!(&layout.file, StonePayloadLayoutFile::Directory(_)))
            .map(|(target, _)| target.as_str())
            .collect::<BTreeSet<_>>(),
        expected.into_iter().collect(),
        "{fixture}: {output} output leaf classification drift"
    );
}

fn assert_no_directories(fixture: &str, output: &str, image: &PackageImage) {
    assert!(
        image
            .layouts
            .values()
            .all(|layout| !matches!(&layout.file, StonePayloadLayoutFile::Directory(_))),
        "{fixture}: {output} unexpectedly emitted a non-empty normal ancestor directory"
    );
}

fn regular_bytes<'a>(fixture: &str, image: &'a PackageImage, target: &str) -> &'a [u8] {
    let layout = image
        .layouts
        .get(target)
        .unwrap_or_else(|| panic!("{fixture}: missing regular layout /usr/{target}"));
    let StonePayloadLayoutFile::Regular(digest, _) = &layout.file else {
        panic!("{fixture}: /usr/{target} is not a regular file")
    };
    image.content[digest].as_slice()
}

fn assert_regular(fixture: &str, image: &PackageImage, target: &str, permissions: u32, expected: Vec<u8>) {
    assert_eq!(
        image.layouts[target].mode & 0o777,
        permissions,
        "{fixture}: /usr/{target} permissions drift"
    );
    assert_eq!(regular_bytes(fixture, image, target), expected);
}

fn assert_symlink(fixture: &str, image: &PackageImage, target: &str, expected_source: &str) {
    let StonePayloadLayoutFile::Symlink(source, _) = &image.layouts[target].file else {
        panic!("{fixture}: /usr/{target} is not a symlink")
    };
    assert_eq!(
        source.as_str(),
        expected_source,
        "{fixture}: /usr/{target} symlink source drift"
    );
    assert_eq!(image.layouts[target].mode & 0o777, 0o777);
}
