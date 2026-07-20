fn assert_go_module_fixture(planned: &super::super::Planned, packages: &BTreeMap<String, PackageImage>) {
    const FIXTURE: &str = "go-module";
    const TREE: &str = "cast-go-module-fixture-1.0.0";
    const EXECUTABLE: &str = "bin/cast-go-module-fixture";
    const LICENSE: &str = "share/licenses/cast-go-module-fixture/LICENSE";
    const IDENTITY: &[u8] = b"cast go module fixture: vendored dependency v0.1.0: declarative userspace";

    assert_eq!(packages.len(), 1, "{FIXTURE}: emitted bundle must contain exactly one package");
    let (output_name, root) = packages.first_key_value().unwrap();
    assert_eq!(output_name.as_str(), "cast-go-module-fixture");
    let [root_plan] = planned.plan.outputs.as_slice() else {
        panic!("{FIXTURE}: frozen plan must contain exactly one output");
    };
    assert_eq!(root_plan.name, "out");
    assert_eq!(root_plan.package_name, *output_name);
    assert!(root_plan.include_in_manifest);
    assert!(root_plan.runtime_inputs.is_empty());
    assert_eq!(root_plan.summary.as_deref(), Some("Pinned offline Go module fixture"));
    assert_eq!(
        root_plan.description.as_deref(),
        Some("A reproducible static Go executable built exclusively from an exact self-authored vendor tree.")
    );

    assert_leaf_paths(FIXTURE, "out", root, [EXECUTABLE, LICENSE]);
    assert_no_directories(FIXTURE, "out", root);
    assert_eq!(root.layouts[EXECUTABLE].mode & 0o777, 0o755);
    let executable = regular_bytes(FIXTURE, root, EXECUTABLE);
    assert_regular(FIXTURE, root, LICENSE, 0o644, tracked_bytes(TREE, "LICENSE"));
    assert!(
        executable.windows(IDENTITY.len()).any(|window| window == IDENTITY),
        "{FIXTURE}: static executable does not contain the exact vendored self-test identity"
    );

    let elf = parse_structural_elf(FIXTURE, EXECUTABLE, executable);
    assert_eq!(elf.ehdr.e_type, ET_EXEC, "{FIXTURE}: static Go executable is not ET_EXEC");
    assert_ne!(elf.ehdr.e_entry, 0, "{FIXTURE}: static Go executable has no entry point");
    let segments = elf
        .segments()
        .unwrap_or_else(|| panic!("{FIXTURE}: static Go executable has no program headers"))
        .iter()
        .collect::<Vec<_>>();
    assert!(
        segments.iter().any(|segment| segment.p_type == PT_LOAD),
        "{FIXTURE}: static Go executable has no PT_LOAD segment"
    );
    assert!(
        segments.iter().all(|segment| segment.p_type != PT_INTERP),
        "{FIXTURE}: static Go executable unexpectedly has PT_INTERP"
    );
    for segment in &segments {
        assert!(segment.p_memsz >= segment.p_filesz, "{FIXTURE}: ELF segment geometry is invalid");
        assert!(
            segment.p_align <= 1 || segment.p_align.is_power_of_two(),
            "{FIXTURE}: ELF segment alignment is invalid"
        );
        if segment.p_type == PT_LOAD {
            assert_ne!(
                segment.p_flags & (PF_W | PF_X),
                PF_W | PF_X,
                "{FIXTURE}: static Go executable has a writable-executable PT_LOAD"
            );
        }
        elf.segment_data(segment)
            .unwrap_or_else(|error| panic!("{FIXTURE}: ELF segment escapes the file: {error}"));
    }

    for forbidden in [
        ".interp",
        ".dynamic",
        ".dynsym",
        ".note.go.buildid",
        ".note.gnu.build-id",
        ".gnu_debuglink",
        ".symtab",
    ] {
        assert!(
            unique_section_by_name(FIXTURE, EXECUTABLE, &elf, forbidden).is_none(),
            "{FIXTURE}: reproducible static executable retained forbidden section {forbidden}"
        );
    }
    let (section_headers, section_names) = elf.section_headers_with_strtab().unwrap();
    let section_headers = section_headers.unwrap();
    let section_names = section_names.unwrap();
    for section in section_headers.iter() {
        let name = section_names.get(usize::try_from(section.sh_name).unwrap()).unwrap();
        assert!(
            !name.starts_with(".debug") && !name.starts_with(".zdebug"),
            "{FIXTURE}: stripped static executable retained debug section {name}"
        );
    }

    for forbidden in [
        "share/doc/cast-go-module-fixture/README.md",
        "share/cast-go-module-fixture/go.mod",
        "share/cast-go-module-fixture/go.sum",
        "share/cast-go-module-fixture/vendor/modules.txt",
    ] {
        assert!(
            !root.layouts.contains_key(forbidden),
            "{FIXTURE}: build-only module data leaked into immutable output: {forbidden}"
        );
    }
    assert_exact_relations(
        FIXTURE,
        root,
        planned_output_dependencies(planned, root_plan),
        BTreeSet::from([
            root_plan.package_name.clone(),
            "binary(cast-go-module-fixture)".to_owned(),
        ]),
    );
}
