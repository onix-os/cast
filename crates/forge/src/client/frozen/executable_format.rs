fn parse_frozen_shebang(probe: &[u8]) -> Result<Option<FrozenShebangInterpreter>, FrozenShebangParseError> {
    if !probe.starts_with(b"#!") {
        return Ok(None);
    }
    let newline = probe.iter().position(|byte| *byte == b'\n').ok_or({
        if probe.len() > MAX_FROZEN_SHEBANG_LINE_BYTES {
            FrozenShebangParseError::LineTooLong
        } else {
            FrozenShebangParseError::Unterminated
        }
    })?;
    if newline + 1 > MAX_FROZEN_SHEBANG_LINE_BYTES {
        return Err(FrozenShebangParseError::LineTooLong);
    }
    let interpreter = &probe[2..newline];
    if interpreter.is_empty() {
        return Err(FrozenShebangParseError::EmptyInterpreter);
    }
    if interpreter.len() > MAX_FROZEN_SHEBANG_INTERPRETER_BYTES {
        return Err(FrozenShebangParseError::InterpreterTooLong);
    }
    if interpreter.contains(&0) {
        return Err(FrozenShebangParseError::Nul);
    }
    if interpreter.iter().any(|byte| byte.is_ascii_whitespace()) {
        return Err(FrozenShebangParseError::WhitespaceOrOptions);
    }
    if interpreter.first() != Some(&b'/') {
        return Err(FrozenShebangParseError::Relative);
    }
    let interpreter = std::str::from_utf8(interpreter).map_err(|_| FrozenShebangParseError::NonUtf8)?;
    let interpreter = normalize_frozen_interpreter_path(interpreter).ok_or(FrozenShebangParseError::NonNormalized)?;
    if interpreter.path == Path::new("/usr/bin/env") {
        return Err(FrozenShebangParseError::EnvironmentLookup);
    }
    Ok(Some(interpreter))
}

fn normalize_frozen_interpreter_path(path: &str) -> Option<FrozenShebangInterpreter> {
    if !path.starts_with('/')
        || path.as_bytes().contains(&0)
        || path.ends_with('/')
        || path.contains("//")
        || path.split('/').any(|component| component == "." || component == "..")
    {
        return None;
    }

    let mut canonical = path.to_owned();
    let mut root_alias = None;
    for (source, target) in ROOT_ABI_LINKS {
        let alias = format!("/{target}");
        if path == alias || path.strip_prefix(&alias).is_some_and(|suffix| suffix.starts_with('/')) {
            canonical = format!("/{source}{}", &path[alias.len()..]);
            root_alias = Some(ExpectedFrozenRootAlias {
                path: PathBuf::from(alias),
                target: source.to_owned(),
            });
            break;
        }
    }
    is_normalized_frozen_path(&canonical).then(|| FrozenShebangInterpreter {
        path: PathBuf::from(canonical),
        root_alias,
    })
}

fn inspect_frozen_executable_format(
    file: &fs::File,
    length: u64,
    probe: &[u8],
    deadline: Instant,
    binding: &FrozenExecutableBinding,
) -> Result<Option<FrozenExecutableInterpreter>, Error> {
    require_frozen_executable_deadline(deadline)?;
    if probe.starts_with(b"#!") {
        return parse_frozen_shebang(probe)
            .map(|interpreter| interpreter.map(FrozenExecutableInterpreter::Shebang))
            .map_err(|source| Error::InvalidFrozenShebang {
                package: binding.package.clone(),
                path: binding.path.clone(),
                reason: source.reason(),
            });
    }
    if !probe.starts_with(b"\x7fELF") {
        return Err(invalid_frozen_executable_format(
            binding,
            "unsupported executable magic; only strict ELF and shebang scripts are admitted",
        ));
    }
    inspect_frozen_elf(file, length, probe, deadline, binding)
}

fn inspect_frozen_elf(
    file: &fs::File,
    length: u64,
    probe: &[u8],
    deadline: Instant,
    binding: &FrozenExecutableBinding,
) -> Result<Option<FrozenExecutableInterpreter>, Error> {
    const ELFCLASS32: u8 = 1;
    const ELFCLASS64: u8 = 2;
    const ELFDATA2LSB: u8 = 1;
    const ELFDATA2MSB: u8 = 2;
    const ET_EXEC: u16 = 2;
    const ET_DYN: u16 = 3;
    const PT_LOAD: u32 = 1;
    const PT_INTERP: u32 = 3;
    const PF_X: u32 = 1;

    require_frozen_executable_deadline(deadline)?;
    if probe.len() < 16 || probe.get(6) != Some(&1) {
        return Err(invalid_frozen_executable_format(
            binding,
            "invalid ELF identification header",
        ));
    }
    let class = probe[4];
    let data = probe[5];
    let expected_class = if usize::BITS == 64 { ELFCLASS64 } else { ELFCLASS32 };
    let expected_data = if cfg!(target_endian = "little") {
        ELFDATA2LSB
    } else {
        ELFDATA2MSB
    };
    if class != expected_class || data != expected_data {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF class or byte order does not match the build host",
        ));
    }
    let little_endian = data == ELFDATA2LSB;
    let (header_size, program_header_size) = match class {
        ELFCLASS32 => (52usize, 32usize),
        ELFCLASS64 => (64usize, 56usize),
        _ => {
            return Err(invalid_frozen_executable_format(binding, "unsupported ELF class"));
        }
    };
    if length < header_size as u64 || probe.len() < header_size {
        return Err(invalid_frozen_executable_format(binding, "truncated ELF header"));
    }

    let elf_type = frozen_elf_u16(probe, 16, little_endian);
    let machine = frozen_elf_u16(probe, 18, little_endian);
    let version = frozen_elf_u32(probe, 20, little_endian);
    if !matches!(elf_type, Some(ET_EXEC) | Some(ET_DYN)) || version != Some(1) {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF is not an executable or position-independent executable",
        ));
    }
    let Some(machine) = machine else {
        return Err(invalid_frozen_executable_format(binding, "truncated ELF machine field"));
    };
    if Some(machine) != native_frozen_elf_machine() {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF machine does not match the build host",
        ));
    }

    let (entry, program_offset, encoded_header_size, encoded_program_header_size, program_count) =
        if class == ELFCLASS64 {
            (
                frozen_elf_u64(probe, 24, little_endian),
                frozen_elf_u64(probe, 32, little_endian),
                frozen_elf_u16(probe, 52, little_endian),
                frozen_elf_u16(probe, 54, little_endian),
                frozen_elf_u16(probe, 56, little_endian),
            )
        } else {
            (
                frozen_elf_u32(probe, 24, little_endian).map(u64::from),
                frozen_elf_u32(probe, 28, little_endian).map(u64::from),
                frozen_elf_u16(probe, 40, little_endian),
                frozen_elf_u16(probe, 42, little_endian),
                frozen_elf_u16(probe, 44, little_endian),
            )
        };
    let (
        Some(entry),
        Some(program_offset),
        Some(encoded_header_size),
        Some(encoded_program_header_size),
        Some(program_count),
    ) = (
        entry,
        program_offset,
        encoded_header_size,
        encoded_program_header_size,
        program_count,
    )
    else {
        return Err(invalid_frozen_executable_format(binding, "truncated ELF header fields"));
    };
    let program_count = usize::from(program_count);
    if program_count > MAX_FROZEN_ELF_PROGRAM_HEADERS {
        return Err(Error::FrozenElfProgramHeaderLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_FROZEN_ELF_PROGRAM_HEADERS,
            actual: program_count,
        });
    }
    if program_count == 0
        || usize::from(encoded_header_size) != header_size
        || usize::from(encoded_program_header_size) != program_header_size
        || program_offset < header_size as u64
    {
        return Err(invalid_frozen_executable_format(
            binding,
            "invalid ELF program-header geometry",
        ));
    }
    let program_bytes = program_count
        .checked_mul(program_header_size)
        .ok_or_else(|| invalid_frozen_executable_format(binding, "ELF program-header table overflows"))?;
    let program_end = program_offset
        .checked_add(program_bytes as u64)
        .ok_or_else(|| invalid_frozen_executable_format(binding, "ELF program-header table overflows"))?;
    if program_end > length {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF program-header table extends beyond the file",
        ));
    }
    let mut program_headers = vec![0u8; program_bytes];
    read_frozen_executable_at(file, program_offset, &mut program_headers, deadline, binding)?;

    let mut load_segments = 0usize;
    let mut executable_entry = false;
    let mut interpreter_segment = None;
    for header in program_headers.chunks_exact(program_header_size) {
        require_frozen_executable_deadline(deadline)?;
        let segment_type = frozen_elf_u32(header, 0, little_endian)
            .ok_or_else(|| invalid_frozen_executable_format(binding, "truncated ELF program header"))?;
        let (flags, offset, virtual_address, file_size, memory_size, alignment) = if class == ELFCLASS64 {
            (
                frozen_elf_u32(header, 4, little_endian),
                frozen_elf_u64(header, 8, little_endian),
                frozen_elf_u64(header, 16, little_endian),
                frozen_elf_u64(header, 32, little_endian),
                frozen_elf_u64(header, 40, little_endian),
                frozen_elf_u64(header, 48, little_endian),
            )
        } else {
            (
                frozen_elf_u32(header, 24, little_endian),
                frozen_elf_u32(header, 4, little_endian).map(u64::from),
                frozen_elf_u32(header, 8, little_endian).map(u64::from),
                frozen_elf_u32(header, 16, little_endian).map(u64::from),
                frozen_elf_u32(header, 20, little_endian).map(u64::from),
                frozen_elf_u32(header, 28, little_endian).map(u64::from),
            )
        };
        let (Some(flags), Some(offset), Some(virtual_address), Some(file_size), Some(memory_size), Some(alignment)) =
            (flags, offset, virtual_address, file_size, memory_size, alignment)
        else {
            return Err(invalid_frozen_executable_format(
                binding,
                "truncated ELF program header",
            ));
        };
        let segment_end = offset
            .checked_add(file_size)
            .ok_or_else(|| invalid_frozen_executable_format(binding, "ELF segment range overflows"))?;
        if segment_end > length || (alignment > 1 && !alignment.is_power_of_two()) {
            return Err(invalid_frozen_executable_format(
                binding,
                "invalid ELF segment bounds or alignment",
            ));
        }
        if segment_type == PT_LOAD {
            if alignment > 1 && offset % alignment != virtual_address % alignment {
                return Err(invalid_frozen_executable_format(binding, "misaligned ELF load mapping"));
            }
            if memory_size < file_size {
                return Err(invalid_frozen_executable_format(
                    binding,
                    "ELF load segment is smaller in memory than in the file",
                ));
            }
            load_segments = load_segments.saturating_add(1);
            let memory_end = virtual_address
                .checked_add(memory_size)
                .ok_or_else(|| invalid_frozen_executable_format(binding, "ELF memory range overflows"))?;
            if flags & PF_X != 0 && entry >= virtual_address && entry < memory_end {
                executable_entry = true;
            }
        }
        if segment_type == PT_INTERP {
            if interpreter_segment.replace((offset, file_size)).is_some() {
                return Err(invalid_frozen_executable_format(
                    binding,
                    "ELF has multiple PT_INTERP segments",
                ));
            }
        }
    }
    if load_segments == 0 || !executable_entry {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF has no executable load segment containing its entry point",
        ));
    }

    let Some((interpreter_offset, interpreter_size)) = interpreter_segment else {
        return Ok(None);
    };
    let interpreter_size = usize::try_from(interpreter_size)
        .map_err(|_| invalid_frozen_executable_format(binding, "ELF PT_INTERP size does not fit in memory"))?;
    if !(2..=MAX_FROZEN_ELF_INTERPRETER_BYTES).contains(&interpreter_size) {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF PT_INTERP path has an invalid length",
        ));
    }
    let mut interpreter = vec![0u8; interpreter_size];
    read_frozen_executable_at(file, interpreter_offset, &mut interpreter, deadline, binding)?;
    if interpreter.last() != Some(&0) || interpreter[..interpreter.len() - 1].contains(&0) {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF PT_INTERP is not one NUL-terminated path",
        ));
    }
    interpreter.pop();
    let interpreter = std::str::from_utf8(&interpreter)
        .ok()
        .and_then(normalize_frozen_interpreter_path)
        .ok_or_else(|| {
            invalid_frozen_executable_format(binding, "ELF PT_INTERP path is not absolute and normalized")
        })?;
    if interpreter.path == Path::new("/usr/bin/env") {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF PT_INTERP environment lookup is forbidden",
        ));
    }
    Ok(Some(FrozenExecutableInterpreter::Elf(interpreter)))
}

fn invalid_frozen_executable_format(binding: &FrozenExecutableBinding, reason: &'static str) -> Error {
    Error::InvalidFrozenExecutableFormat {
        package: binding.package.clone(),
        path: binding.path.clone(),
        reason,
    }
}

fn native_frozen_elf_machine() -> Option<u16> {
    match std::env::consts::ARCH {
        "x86" => Some(3),
        "mips" | "mips64" => Some(8),
        "powerpc" => Some(20),
        "powerpc64" => Some(21),
        "s390x" => Some(22),
        "arm" => Some(40),
        "x86_64" => Some(62),
        "aarch64" => Some(183),
        "riscv32" | "riscv64" => Some(243),
        _ => None,
    }
}

fn frozen_elf_u16(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u16> {
    let bytes: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(if little_endian {
        u16::from_le_bytes(bytes)
    } else {
        u16::from_be_bytes(bytes)
    })
}

fn frozen_elf_u32(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u32> {
    let bytes: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(if little_endian {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    })
}

fn frozen_elf_u64(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u64> {
    let bytes: [u8; 8] = bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(if little_endian {
        u64::from_le_bytes(bytes)
    } else {
        u64::from_be_bytes(bytes)
    })
}

fn read_frozen_executable_at(
    file: &fs::File,
    offset: u64,
    output: &mut [u8],
    deadline: Instant,
    binding: &FrozenExecutableBinding,
) -> Result<(), Error> {
    let mut read = 0usize;
    while read < output.len() {
        require_frozen_executable_deadline(deadline)?;
        let position = offset
            .checked_add(read as u64)
            .and_then(|position| i64::try_from(position).ok())
            .ok_or_else(|| invalid_frozen_executable_format(binding, "ELF read offset overflows"))?;
        // SAFETY: `file` remains live, `output[read..]` is writable, and the
        // checked offset is representable as off_t on supported Linux hosts.
        let result = unsafe {
            nix::libc::pread(
                file.as_raw_fd(),
                output[read..].as_mut_ptr().cast(),
                output.len() - read,
                position,
            )
        };
        if result < 0 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(Error::ReadFrozenExecutable {
                package: binding.package.clone(),
                path: binding.path.clone(),
                source,
            });
        }
        let result = usize::try_from(result).map_err(|_| Error::ReadFrozenExecutable {
            package: binding.package.clone(),
            path: binding.path.clone(),
            source: io::Error::other("pread returned a negative byte count"),
        })?;
        if result == 0 {
            return Err(invalid_frozen_executable_format(
                binding,
                "ELF changed or ended during inspection",
            ));
        }
        read += result;
    }
    Ok(())
}

fn verify_frozen_executable<F>(
    root: &fs::File,
    binding: &FrozenExecutableBinding,
    expected: ExpectedFrozenExecutable,
    deadline: Instant,
    total_bytes: &mut u64,
    checkpoint: &mut F,
) -> Result<(Option<FrozenExecutableInterpreter>, PinnedFrozenExecutable), Error>
where
    F: FnMut(&FrozenExecutableBinding, FrozenExecutableCheckpoint),
{
    require_frozen_executable_deadline(deadline)?;
    let pinned_symlinks = expected
        .symlinks
        .iter()
        .map(|symlink| pin_frozen_symlink(root, symlink))
        .collect::<Result<Vec<_>, Error>>()?;
    let mut file = open_frozen_executable(root, binding, &expected.resolved_path)?;
    let before = frozen_executable_witness(&file, binding)?;
    require_frozen_executable_metadata(binding, &expected, before)?;
    account_frozen_executable_bytes(binding, before.length, total_bytes)?;

    checkpoint(binding, FrozenExecutableCheckpoint::AfterOpen);
    let inspected = digest_frozen_executable(&mut file, before.length, deadline, binding)?;
    checkpoint(binding, FrozenExecutableCheckpoint::AfterDigest);
    let after = frozen_executable_witness(&file, binding)?;
    if after != before {
        return Err(Error::FrozenExecutableChanged {
            package: binding.package.clone(),
            path: binding.path.clone(),
        });
    }
    if inspected.digest != expected.digest {
        return Err(Error::FrozenExecutableDigestMismatch {
            package: binding.package.clone(),
            path: binding.path.clone(),
            expected: expected.digest,
            actual: inspected.digest,
        });
    }
    let interpreter =
        inspect_frozen_executable_format(&file, before.length, &inspected.shebang_probe, deadline, binding)?;

    checkpoint(binding, FrozenExecutableCheckpoint::BeforeReopen);
    let reopened = open_frozen_executable(root, binding, &expected.resolved_path)?;
    let named = frozen_executable_witness(&reopened, binding)?;
    if named != before {
        return Err(Error::FrozenExecutablePathReplaced {
            package: binding.package.clone(),
            path: binding.path.clone(),
        });
    }
    for symlink in &pinned_symlinks {
        require_pinned_frozen_symlink(root, symlink)?;
    }

    Ok((
        interpreter,
        PinnedFrozenExecutable {
            file,
            witness: before,
            binding: binding.clone(),
            expected,
            symlinks: pinned_symlinks,
        },
    ))
}
