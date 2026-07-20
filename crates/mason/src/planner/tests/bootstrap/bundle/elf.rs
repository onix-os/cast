use elf::abi::{
    DF_1_NOW, DF_BIND_NOW, DF_TEXTREL, DT_BIND_NOW, DT_FLAGS, DT_FLAGS_1, DT_RPATH, DT_RUNPATH, DT_TEXTREL, PF_W,
    PF_X, PT_GNU_RELRO, PT_GNU_STACK,
};

#[derive(Debug, Clone, Copy)]
enum RuntimeElfKind {
    Executable,
    SharedLibrary,
}

#[derive(Debug)]
struct DebugLink {
    basename: String,
    crc32: u32,
}

#[derive(Debug)]
struct NativeElf {
    elf_type: u16,
    build_id: String,
    dependencies: BTreeSet<String>,
    soname: Option<String>,
    undefined_dynamic_imports: BTreeSet<String>,
    has_gnu_relro: bool,
    immediate_binding: bool,
    gnu_stack_executable: Option<bool>,
    has_rpath_or_runpath: bool,
    has_text_relocations: bool,
    debug_link: Option<DebugLink>,
}

#[derive(Debug)]
struct RuntimeElfDynamic {
    dependencies: BTreeSet<String>,
    soname: Option<String>,
    undefined_imports: BTreeSet<String>,
    immediate_binding: bool,
    has_rpath_or_runpath: bool,
    has_text_relocations: bool,
}

fn parse_structural_elf<'a>(fixture: &str, target: &str, bytes: &'a [u8]) -> ElfBytes<'a, AnyEndian> {
    let elf = ElfBytes::<AnyEndian>::minimal_parse(bytes)
        .unwrap_or_else(|error| panic!("{fixture}: structurally parse /usr/{target} as ELF: {error}"));
    assert_eq!(elf.ehdr.class, Class::ELF64, "{fixture}: /usr/{target} is not ELF64");
    assert!(
        elf.ehdr.endianness.is_little(),
        "{fixture}: /usr/{target} is not a little-endian ELF"
    );
    assert_eq!(
        elf.ehdr.version,
        u32::from(EV_CURRENT),
        "{fixture}: /usr/{target} has an unsupported ELF version"
    );
    assert_eq!(
        elf.ehdr.e_machine, EM_X86_64,
        "{fixture}: /usr/{target} is not an x86_64 ELF"
    );
    let sections = elf
        .section_headers()
        .unwrap_or_else(|| panic!("{fixture}: /usr/{target} has no section-header table"));
    assert!(
        sections.iter().count() > 1,
        "{fixture}: /usr/{target} has no meaningful ELF sections"
    );
    // Resolve the section-name table too; minimal_parse intentionally leaves
    // both tables lazy, so this turns malformed table geometry into a failure.
    elf.section_headers_with_strtab()
        .unwrap_or_else(|error| panic!("{fixture}: parse /usr/{target} section table and names: {error}"));
    elf
}

fn unique_section_by_name(
    fixture: &str,
    target: &str,
    elf: &ElfBytes<'_, AnyEndian>,
    expected: &str,
) -> Option<elf::section::SectionHeader> {
    let (headers, names) = elf.section_headers_with_strtab().unwrap_or_else(|error| {
        panic!("{fixture}: parse /usr/{target} section names while finding {expected}: {error}")
    });
    let headers = headers.unwrap_or_else(|| panic!("{fixture}: /usr/{target} has no section-header table"));
    let names = names.unwrap_or_else(|| panic!("{fixture}: /usr/{target} has no section-name string table"));
    let mut found = None;
    for section in headers.iter() {
        let name = names
            .get(usize::try_from(section.sh_name).unwrap())
            .unwrap_or_else(|error| {
                panic!("{fixture}: resolve /usr/{target} section name while finding {expected}: {error}")
            });
        if name == expected {
            assert!(
                found.replace(section).is_none(),
                "{fixture}: /usr/{target} repeats ELF section {expected}"
            );
        }
    }
    found
}

fn canonical_interpreter<'a>(fixture: &str, target: &str, data: &'a [u8]) -> &'a str {
    let interpreter = CStr::from_bytes_until_nul(data)
        .unwrap_or_else(|error| panic!("{fixture}: /usr/{target} interpreter is not NUL-terminated: {error}"));
    assert_eq!(
        interpreter.to_bytes_with_nul().len(),
        data.len(),
        "{fixture}: /usr/{target} interpreter contains bytes after its terminator"
    );
    let interpreter = interpreter
        .to_str()
        .unwrap_or_else(|error| panic!("{fixture}: /usr/{target} interpreter is not UTF-8: {error}"));
    let relative = interpreter.strip_prefix('/').unwrap_or("");
    assert!(
        !relative.is_empty()
            && !interpreter.bytes().any(|byte| byte.is_ascii_control())
            && relative
                .split('/')
                .all(|component| !component.is_empty() && !matches!(component, "." | "..")),
        "{fixture}: /usr/{target} interpreter is not a canonical absolute path"
    );
    interpreter
}

#[test]
fn interpreter_path_must_be_exactly_normalized() {
    assert_eq!(
        canonical_interpreter("interp", "tool", b"/usr/lib/ld-linux-x86-64.so.2\0"),
        "/usr/lib/ld-linux-x86-64.so.2"
    );
    for malformed in [
        &b"relative\0"[..],
        &b"/\0"[..],
        &b"/usr//lib/ld.so\0"[..],
        &b"/usr/./lib/ld.so\0"[..],
        &b"/usr/../lib/ld.so\0"[..],
        &b"/usr/lib/\0"[..],
        &b"/usr/\x01lib/ld.so\0"[..],
        &b"/usr/lib/ld.so\0trailing"[..],
    ] {
        assert!(
            std::panic::catch_unwind(|| canonical_interpreter("interp", "tool", malformed)).is_err(),
            "non-canonical interpreter was accepted: {malformed:?}"
        );
    }
}

fn assert_runtime_elf(
    fixture: &str,
    target: &str,
    bytes: &[u8],
    kind: RuntimeElfKind,
    analysis: &AnalysisPlan,
) -> NativeElf {
    let elf = parse_structural_elf(fixture, target, bytes);
    match kind {
        RuntimeElfKind::Executable => {
            assert!(
                matches!(elf.ehdr.e_type, ET_EXEC | ET_DYN),
                "{fixture}: /usr/{target} is neither ET_EXEC nor a PIE ET_DYN"
            );
            assert_ne!(
                elf.ehdr.e_entry, 0,
                "{fixture}: /usr/{target} has no executable entry point"
            );
        }
        RuntimeElfKind::SharedLibrary => assert_eq!(
            elf.ehdr.e_type, ET_DYN,
            "{fixture}: /usr/{target} shared library is not ET_DYN"
        ),
    }

    let segments = elf
        .segments()
        .unwrap_or_else(|| panic!("{fixture}: runtime ELF /usr/{target} has no program-header table"))
        .iter()
        .collect::<Vec<_>>();
    assert!(
        segments.iter().any(|segment| segment.p_type == PT_LOAD),
        "{fixture}: runtime ELF /usr/{target} has no loadable segment"
    );
    let gnu_relro_segments = segments
        .iter()
        .filter(|segment| segment.p_type == PT_GNU_RELRO)
        .collect::<Vec<_>>();
    assert!(
        gnu_relro_segments.len() <= 1,
        "{fixture}: runtime ELF /usr/{target} repeats PT_GNU_RELRO"
    );
    if let Some(segment) = gnu_relro_segments.first() {
        assert_ne!(
            segment.p_memsz, 0,
            "{fixture}: runtime ELF /usr/{target} has an empty PT_GNU_RELRO"
        );
    }
    let gnu_stack_segments = segments
        .iter()
        .filter(|segment| segment.p_type == PT_GNU_STACK)
        .collect::<Vec<_>>();
    assert!(
        gnu_stack_segments.len() <= 1,
        "{fixture}: runtime ELF /usr/{target} repeats PT_GNU_STACK"
    );
    let gnu_stack_executable = gnu_stack_segments
        .first()
        .map(|segment| segment.p_flags & PF_X != 0);
    for segment in &segments {
        if segment.p_type == PT_LOAD {
            assert_ne!(
                segment.p_flags & (PF_W | PF_X),
                PF_W | PF_X,
                "{fixture}: runtime ELF /usr/{target} has a writable and executable PT_LOAD segment"
            );
        }
        assert!(
            segment.p_memsz >= segment.p_filesz,
            "{fixture}: runtime ELF /usr/{target} has a segment larger on disk than in memory"
        );
        assert!(
            segment.p_align <= 1 || segment.p_align.is_power_of_two(),
            "{fixture}: runtime ELF /usr/{target} has an invalid segment alignment"
        );
        elf.segment_data(segment)
            .unwrap_or_else(|error| panic!("{fixture}: runtime ELF /usr/{target} segment escapes file: {error}"));
    }

    for name in [".debug_info", ".zdebug_info", ".debug_line", ".zdebug_line"] {
        assert!(
            unique_section_by_name(fixture, target, &elf, name).is_none(),
            "{fixture}: stripped runtime ELF /usr/{target} retains {name}"
        );
    }

    let interp_section = unique_section_by_name(fixture, target, &elf, ".interp");
    let interp_segments = segments
        .iter()
        .filter(|segment| segment.p_type == PT_INTERP)
        .collect::<Vec<_>>();
    let interpreter = match kind {
        RuntimeElfKind::Executable => {
            assert_eq!(
                interp_segments.len(),
                1,
                "{fixture}: executable /usr/{target} must have exactly one PT_INTERP program header"
            );
            let section = interp_section
                .as_ref()
                .unwrap_or_else(|| panic!("{fixture}: executable /usr/{target} has no .interp section"));
            let (section_data, compression) = elf
                .section_data(section)
                .unwrap_or_else(|error| panic!("{fixture}: read executable /usr/{target} .interp: {error}"));
            assert!(
                compression.is_none(),
                "{fixture}: executable /usr/{target} has a compressed .interp section"
            );
            let segment_data = elf
                .segment_data(interp_segments[0])
                .unwrap_or_else(|error| panic!("{fixture}: read executable /usr/{target} PT_INTERP segment: {error}"));
            assert_eq!(
                interp_segments[0].p_offset, section.sh_offset,
                "{fixture}: executable /usr/{target} PT_INTERP and .interp offsets differ"
            );
            assert_eq!(
                interp_segments[0].p_filesz, section.sh_size,
                "{fixture}: executable /usr/{target} PT_INTERP and .interp sizes differ"
            );
            assert_eq!(
                interp_segments[0].p_memsz, interp_segments[0].p_filesz,
                "{fixture}: executable /usr/{target} PT_INTERP has distinct file and memory sizes"
            );
            assert_eq!(
                interp_segments[0].p_vaddr, section.sh_addr,
                "{fixture}: executable /usr/{target} PT_INTERP and .interp virtual addresses differ"
            );
            assert_eq!(
                segment_data, section_data,
                "{fixture}: executable /usr/{target} PT_INTERP and .interp bytes differ"
            );
            Some(canonical_interpreter(fixture, target, section_data))
        }
        RuntimeElfKind::SharedLibrary => {
            assert!(
                interp_section.is_none(),
                "{fixture}: shared library /usr/{target} unexpectedly has .interp"
            );
            assert!(
                interp_segments.is_empty(),
                "{fixture}: shared library /usr/{target} unexpectedly has PT_INTERP"
            );
            None
        }
    };

    let build_id = elf_build_id(fixture, target, &elf);
    assert_eq!(
        build_id.len(),
        40,
        "{fixture}: /usr/{target} does not use a SHA-1 GNU build ID"
    );
    assert!(
        build_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{fixture}: /usr/{target} GNU build ID is not lowercase hexadecimal"
    );

    let dynamic = runtime_elf_dynamic(fixture, target, &elf, interpreter);
    if matches!(kind, RuntimeElfKind::SharedLibrary) {
        assert!(
            dynamic.soname.is_some(),
            "{fixture}: shared library /usr/{target} has no DT_SONAME"
        );
    } else {
        assert!(
            dynamic.soname.is_none(),
            "{fixture}: executable /usr/{target} unexpectedly has DT_SONAME"
        );
    }

    NativeElf {
        elf_type: elf.ehdr.e_type,
        build_id,
        dependencies: dynamic.dependencies,
        soname: dynamic.soname,
        undefined_dynamic_imports: dynamic.undefined_imports,
        has_gnu_relro: gnu_relro_segments.len() == 1,
        immediate_binding: dynamic.immediate_binding,
        gnu_stack_executable,
        has_rpath_or_runpath: dynamic.has_rpath_or_runpath,
        has_text_relocations: dynamic.has_text_relocations,
        debug_link: enforce_runtime_debug_link_policy(
            fixture,
            target,
            analysis,
            parse_debug_link(fixture, target, &elf),
        ),
    }
}

fn runtime_elf_dynamic(
    fixture: &str,
    target: &str,
    elf: &ElfBytes<'_, AnyEndian>,
    interpreter: Option<&str>,
) -> RuntimeElfDynamic {
    let mut needed = Vec::new();
    let mut soname = None;
    let mut immediate_binding = false;
    let mut has_rpath_or_runpath = false;
    let mut has_text_relocations = false;
    let dynamic = elf
        .dynamic()
        .unwrap_or_else(|error| panic!("{fixture}: parse /usr/{target} dynamic table: {error}"))
        .unwrap_or_else(|| panic!("{fixture}: runtime ELF /usr/{target} has no dynamic table"));
    for entry in dynamic.iter() {
        match entry.d_tag {
            DT_NEEDED => needed.push(usize::try_from(entry.d_val()).unwrap()),
            DT_SONAME => soname = Some(usize::try_from(entry.d_val()).unwrap()),
            DT_BIND_NOW => immediate_binding = true,
            DT_FLAGS => {
                immediate_binding |= entry.d_val() & u64::try_from(DF_BIND_NOW).unwrap() != 0;
                has_text_relocations |= entry.d_val() & u64::try_from(DF_TEXTREL).unwrap() != 0;
            }
            DT_FLAGS_1 => {
                immediate_binding |= entry.d_val() & u64::try_from(DF_1_NOW).unwrap() != 0;
            }
            DT_RPATH | DT_RUNPATH => has_rpath_or_runpath = true,
            DT_TEXTREL => has_text_relocations = true,
            _ => {}
        }
    }
    let (symbols, strings) = elf
        .dynamic_symbol_table()
        .unwrap_or_else(|error| panic!("{fixture}: parse /usr/{target} dynamic strings: {error}"))
        .unwrap_or_else(|| panic!("{fixture}: runtime ELF /usr/{target} has no dynamic string table"));
    let mut dependencies = needed
        .into_iter()
        .map(|offset| {
            let name = strings
                .get(offset)
                .unwrap_or_else(|error| panic!("{fixture}: resolve /usr/{target} DT_NEEDED: {error}"));
            format!("soname({name}(x86_64))")
        })
        .collect::<BTreeSet<_>>();
    if let Some(interpreter) = interpreter {
        dependencies.insert(format!("interpreter({interpreter}(x86_64))"));
    }
    let soname = soname.map(|offset| {
        strings
            .get(offset)
            .unwrap_or_else(|error| panic!("{fixture}: resolve /usr/{target} DT_SONAME: {error}"))
            .to_owned()
    });
    let undefined_imports = symbols
        .iter()
        .filter(|symbol| symbol.is_undefined() && symbol.st_name != 0)
        .map(|symbol| {
            strings
                .get(usize::try_from(symbol.st_name).unwrap())
                .unwrap_or_else(|error| {
                    panic!("{fixture}: resolve /usr/{target} undefined dynamic symbol: {error}")
                })
                .to_owned()
        })
        .collect();
    RuntimeElfDynamic {
        dependencies,
        soname,
        undefined_imports,
        immediate_binding,
        has_rpath_or_runpath,
        has_text_relocations,
    }
}

fn elf_build_id(fixture: &str, target: &str, elf: &ElfBytes<'_, AnyEndian>) -> String {
    let section = elf
        .section_header_by_name(".note.gnu.build-id")
        .unwrap_or_else(|error| panic!("{fixture}: inspect /usr/{target} GNU build-ID section: {error}"))
        .unwrap_or_else(|| panic!("{fixture}: /usr/{target} has no GNU build-ID section"));
    let notes = elf
        .section_data_as_notes(&section)
        .unwrap_or_else(|error| panic!("{fixture}: parse /usr/{target} GNU build-ID notes: {error}"));
    let build_ids = notes
        .filter_map(|note| match note {
            Note::GnuBuildId(build_id) => Some(hex::encode(build_id.0)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        build_ids.len(),
        1,
        "{fixture}: /usr/{target} must contain exactly one GNU build ID"
    );
    build_ids.into_iter().next().unwrap()
}

fn parse_debug_link(fixture: &str, target: &str, elf: &ElfBytes<'_, AnyEndian>) -> Option<DebugLink> {
    let section = elf
        .section_header_by_name(".gnu_debuglink")
        .unwrap_or_else(|error| panic!("{fixture}: inspect /usr/{target} .gnu_debuglink: {error}"));
    let Some(section) = section else {
        return None;
    };
    let (data, compression) = elf
        .section_data(&section)
        .unwrap_or_else(|error| panic!("{fixture}: read /usr/{target} .gnu_debuglink: {error}"));
    assert!(
        compression.is_none(),
        "{fixture}: /usr/{target} has a compressed .gnu_debuglink"
    );
    let nul = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or_else(|| panic!("{fixture}: /usr/{target} .gnu_debuglink has no NUL terminator"));
    let basename = std::str::from_utf8(&data[..nul])
        .unwrap_or_else(|error| panic!("{fixture}: /usr/{target} .gnu_debuglink basename is not UTF-8: {error}"));
    assert_eq!(
        Path::new(basename).file_name().and_then(|name| name.to_str()),
        Some(basename),
        "{fixture}: /usr/{target} .gnu_debuglink is not a basename"
    );
    let crc_offset = nul
        .checked_add(4)
        .map(|value| value & !3)
        .expect("debuglink padding offset overflow");
    assert_eq!(
        data.len(),
        crc_offset + 4,
        "{fixture}: /usr/{target} .gnu_debuglink has non-canonical padding/cardinality"
    );
    assert!(
        data[nul + 1..crc_offset].iter().all(|byte| *byte == 0),
        "{fixture}: /usr/{target} .gnu_debuglink padding is not zeroed"
    );
    let crc32 = u32::from_le_bytes(data[crc_offset..].try_into().unwrap());
    Some(DebugLink {
        basename: basename.to_owned(),
        crc32,
    })
}

fn enforce_runtime_debug_link_policy(
    fixture: &str,
    target: &str,
    analysis: &AnalysisPlan,
    debug_link: Option<DebugLink>,
) -> Option<DebugLink> {
    match (analysis.debug, debug_link) {
        (true, Some(debug_link)) => Some(debug_link),
        (true, None) => panic!("{fixture}: debug-enabled runtime ELF /usr/{target} has no .gnu_debuglink"),
        (false, None) => None,
        (false, Some(_)) => panic!("{fixture}: debug-disabled runtime ELF /usr/{target} retained .gnu_debuglink"),
    }
}

#[test]
fn runtime_elf_debug_link_is_required_when_plan_debug_is_enabled() {
    let analysis = AnalysisPlan {
        debug: true,
        ..AnalysisPlan::default()
    };
    let accepted = enforce_runtime_debug_link_policy(
        "enabled",
        "bin/tool",
        &analysis,
        Some(DebugLink {
            basename: "tool.debug".to_owned(),
            crc32: 7,
        }),
    )
    .expect("debug-enabled policy rejected an authenticated debug link");
    assert_eq!(accepted.basename, "tool.debug");
    assert_eq!(accepted.crc32, 7);
    assert!(
        std::panic::catch_unwind(|| {
            enforce_runtime_debug_link_policy("enabled", "bin/tool", &analysis, None);
        })
        .is_err(),
        "debug-enabled policy accepted a missing .gnu_debuglink"
    );
}

#[test]
fn runtime_elf_debug_link_is_forbidden_when_plan_debug_is_disabled() {
    let analysis = AnalysisPlan {
        debug: false,
        ..AnalysisPlan::default()
    };
    assert!(enforce_runtime_debug_link_policy("disabled", "bin/tool", &analysis, None).is_none());
    assert!(
        std::panic::catch_unwind(|| {
            enforce_runtime_debug_link_policy(
                "disabled",
                "bin/tool",
                &analysis,
                Some(DebugLink {
                    basename: "tool.debug".to_owned(),
                    crc32: 7,
                }),
            );
        })
        .is_err(),
        "debug-disabled policy accepted a retained .gnu_debuglink"
    );
}

fn assert_debug_elf(fixture: &str, target: &str, bytes: &[u8], original: &NativeElf) {
    let elf = parse_structural_elf(fixture, target, bytes);
    assert_eq!(
        elf.ehdr.e_type, original.elf_type,
        "{fixture}: debug ELF /usr/{target} type differs from its runtime ELF"
    );
    assert_eq!(
        elf_build_id(fixture, target, &elf),
        original.build_id,
        "{fixture}: debug ELF /usr/{target} build ID differs from its runtime ELF"
    );
    for alternatives in [[".debug_info", ".zdebug_info"], [".debug_line", ".zdebug_line"]] {
        let sections = alternatives.map(|name| unique_section_by_name(fixture, target, &elf, name));
        assert_eq!(
            sections.iter().filter(|section| section.is_some()).count(),
            1,
            "{fixture}: debug ELF /usr/{target} must contain exactly one of {} and {}",
            alternatives[0],
            alternatives[1]
        );
        let (name, section) = alternatives
            .into_iter()
            .zip(sections)
            .find_map(|(name, section)| section.map(|section| (name, section)))
            .expect("exactly one debug-section alternative was required");
        assert_debug_section(fixture, target, &elf, name, &section);
    }
}

fn assert_debug_section(
    fixture: &str,
    target: &str,
    elf: &ElfBytes<'_, AnyEndian>,
    name: &str,
    section: &elf::section::SectionHeader,
) {
    assert_eq!(
        section.sh_type, SHT_PROGBITS,
        "{fixture}: debug ELF /usr/{target} section {name} is not SHT_PROGBITS"
    );
    assert_ne!(
        section.sh_size, 0,
        "{fixture}: debug ELF /usr/{target} section {name} is empty"
    );
    let (data, compression) = elf
        .section_data(section)
        .unwrap_or_else(|error| panic!("{fixture}: read debug ELF /usr/{target} section {name}: {error}"));
    assert!(
        !data.is_empty(),
        "{fixture}: debug ELF /usr/{target} section {name} has no stored data"
    );

    if name.starts_with(".zdebug_") {
        assert!(
            compression.is_none(),
            "{fixture}: GNU-compressed debug section /usr/{target}:{name} also uses SHF_COMPRESSED"
        );
        assert!(
            data.len() > 12 && data.starts_with(b"ZLIB"),
            "{fixture}: GNU-compressed debug section /usr/{target}:{name} has no valid ZLIB envelope"
        );
        let plain_size = u64::from_be_bytes(data[4..12].try_into().unwrap());
        assert!(
            plain_size >= 6,
            "{fixture}: GNU-compressed debug section /usr/{target}:{name} declares no DWARF unit"
        );
    } else if let Some(compression) = compression {
        assert!(
            matches!(compression.ch_type, ELFCOMPRESS_ZLIB | ELFCOMPRESS_ZSTD),
            "{fixture}: debug section /usr/{target}:{name} uses an unknown compression type"
        );
        assert!(
            compression.ch_size >= 6,
            "{fixture}: compressed debug section /usr/{target}:{name} declares no DWARF unit"
        );
        assert!(
            compression.ch_addralign <= 1 || compression.ch_addralign.is_power_of_two(),
            "{fixture}: compressed debug section /usr/{target}:{name} has invalid alignment"
        );
    } else {
        assert_dwarf_unit_prefix(fixture, target, name, data);
    }
}

fn assert_dwarf_unit_prefix(fixture: &str, target: &str, name: &str, data: &[u8]) {
    assert!(
        data.len() >= 6,
        "{fixture}: debug section /usr/{target}:{name} is too short for a DWARF unit"
    );
    let initial = u32::from_le_bytes(data[..4].try_into().unwrap());
    let (prefix, body_size, version_offset) = if initial == u32::MAX {
        assert!(
            data.len() >= 14,
            "{fixture}: debug section /usr/{target}:{name} has a truncated DWARF64 unit"
        );
        (12usize, u64::from_le_bytes(data[4..12].try_into().unwrap()), 12usize)
    } else {
        assert!(
            initial < 0xffff_fff0,
            "{fixture}: debug section /usr/{target}:{name} uses a reserved DWARF initial length"
        );
        (4usize, u64::from(initial), 4usize)
    };
    assert!(
        body_size >= 2,
        "{fixture}: debug section /usr/{target}:{name} declares a DWARF unit without a version"
    );
    let total = u64::try_from(prefix)
        .unwrap()
        .checked_add(body_size)
        .expect("DWARF unit length overflow");
    assert!(
        total <= u64::try_from(data.len()).unwrap(),
        "{fixture}: debug section /usr/{target}:{name} has a truncated DWARF unit"
    );
    let version = u16::from_le_bytes(data[version_offset..version_offset + 2].try_into().unwrap());
    assert!(
        (2..=5).contains(&version),
        "{fixture}: debug section /usr/{target}:{name} has unsupported DWARF version {version}"
    );
}

#[test]
fn dwarf_debug_section_prefix_must_be_nonempty_and_bounded() {
    assert_dwarf_unit_prefix("dwarf", "debug", ".debug_info", &[2, 0, 0, 0, 5, 0]);
    for malformed in [&[][..], &[0, 0, 0, 0, 5, 0], &[8, 0, 0, 0, 5, 0]] {
        assert!(
            std::panic::catch_unwind(|| assert_dwarf_unit_prefix("dwarf", "debug", ".debug_info", malformed)).is_err(),
            "malformed DWARF prefix was accepted: {malformed:?}"
        );
    }
}

fn debug_target(build_id: &str) -> String {
    assert!(
        build_id.len() >= 2,
        "GNU build ID is too short for the build-ID hierarchy"
    );
    format!("lib/debug/.build-id/{}/{}.debug", &build_id[..2], &build_id[2..])
}

fn gnu_debuglink_crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let polynomial = 0xedb8_8320 & 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ polynomial;
        }
    }
    !crc
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
}
