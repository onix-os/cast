// SPDX-FileCopyrightText: 2024 AerynOS Developers

use std::{
    ffi::CStr,
    path::{Path, PathBuf},
};

use elf::{
    abi::{DT_NEEDED, DT_RPATH, DT_RUNPATH, DT_SONAME, ET_DYN},
    endian::AnyEndian,
    file::Class,
    note::Note,
    to_str,
};
use fs_err::File;
use path_clean::clean;

use stone::relation::{Dependency, Kind, Provider};

use super::{ExternalAnalyzerMutation, VerifiedAnalyzerInput, analyzer_command, checked_output_for};
use crate::package::{
    analysis::{BoxError, BucketMut, Decision, Response},
    collect::{GeneratedArtifact, PathInfo},
};

pub fn elf(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    let file_name = info.file_name();

    if file_name.ends_with(".debug") && info.has_component("debug") {
        return Ok(Decision::NextHandler.into());
    }
    if !info.is_file() {
        return Ok(Decision::NextHandler.into());
    }

    let Ok(mut elf) = parse_elf(&info.path) else {
        return Ok(Decision::NextHandler.into());
    };

    let machine_isa = to_str::e_machine_to_str(elf.ehdr.e_machine)
        .map(|s| s.strip_prefix("EM_").unwrap_or(s))
        .unwrap_or_default()
        .to_lowercase();
    let bit_size = elf.ehdr.class;
    let has_interp = elf.section_header_by_name(".interp").ok().flatten().is_some();

    // A package-private dynamic loader satisfies the exact PT_INTERP path used
    // by executables in the same stone. musl's loader advertises a libc SONAME,
    // so it cannot be identified reliably from its file name or SONAME alone.
    // Restrict this provider to executable ET_DYN objects without an interpreter
    // of their own: dynamic loaders have that shape, while PIE executables and
    // ordinary shared libraries do not.
    if is_interpreter_candidate(elf.ehdr.e_type, info.layout.mode, has_interp) {
        bucket.providers.insert(Provider {
            kind: Kind::Interpreter,
            name: format!("{}({machine_isa})", info.target_path.display()),
        });
    }

    parse_dynamic_section(&mut elf, bucket, &machine_isa, bit_size, info, file_name);
    parse_interp_section(&mut elf, bucket, &machine_isa);

    let build_id = parse_build_id(&mut elf);

    let mut publications = vec![];

    if let Some(build_id) = build_id {
        let debug_destination = bucket
            .analysis
            .debug
            .then(|| pending_debug_destination(info, bit_size, &build_id))
            .transpose()?
            .flatten();
        if !bucket.analysis.strip && debug_destination.is_none() {
            return Ok(Response {
                decision: Decision::IncludeFile,
                publications,
            });
        }
        let byte_limit = info.regular_file_byte_limit()?;
        let input = VerifiedAnalyzerInput::from_path_info(info, info.size)?;
        let sandbox = ExternalAnalyzerMutation::new(&input, &info.target_path, "input.elf", ".elf-mutation")?;
        let debug_output = debug_destination
            .as_ref()
            .map(|path| {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("build-id debug output has a validated UTF-8 basename");
                sandbox.output_path(name).map(|private| (name, private))
            })
            .transpose()?;
        let operation = (|| {
            if let Some((_, output)) = &debug_output {
                split_debug(bucket, info, sandbox.path(), output)?;
            }
            strip(bucket, info, sandbox.path())
        })();
        let (replacement, debug_bytes) = match &debug_output {
            Some((name, _)) => {
                let (replacement, debug) = sandbox.finish_with_output(info, operation, byte_limit, name)?;
                (replacement, Some(debug))
            }
            None => (sandbox.finish(info, operation, byte_limit)?, None),
        };
        info.replace_regular_from(&replacement)?;
        if let Some((destination, bytes)) = debug_destination.zip(debug_bytes) {
            publications.push(GeneratedArtifact::regular(destination, bytes, 0o644, None, true));
        }
    }

    Ok(Response {
        decision: Decision::IncludeFile,
        publications,
    })
}

fn is_interpreter_candidate(elf_type: u16, mode: u32, has_interp: bool) -> bool {
    elf_type == ET_DYN && mode & 0o111 != 0 && !has_interp
}

fn package_private_rpath_dependency(rpath: &str, name: &str) -> Option<PathBuf> {
    if !rpath.starts_with("/usr/lib/onix/stones/") || !rpath.ends_with("/lib") || name.contains('/') {
        return None;
    }

    Some(Path::new(rpath).components().skip(3).collect::<PathBuf>().join(name))
}

#[cfg(test)]
mod tests {
    use super::{is_interpreter_candidate, package_private_rpath_dependency};
    use elf::abi::{ET_DYN, ET_EXEC};
    use std::{
        collections::BTreeSet,
        os::unix::fs::{MetadataExt, PermissionsExt},
        path::{Path, PathBuf},
    };

    use super::{elf, parse_build_id, parse_elf};
    use crate::{
        Paths, Recipe,
        package::{
            analysis::{BucketMut, Decision},
            collect::Collector,
            test_derivation_plan,
        },
    };
    use stone::StoneDigestWriterHasher;
    use stone::relation::Kind as RelationKind;
    use stone_recipe::derivation::{ExecutablePlan, PathRuleKind, RelationPlan};

    fn build_id_fixture_path() -> PathBuf {
        let mut candidates = vec![PathBuf::from("/bin/sh"), PathBuf::from("/usr/bin/env")];
        candidates.push(std::env::current_exe().unwrap());

        candidates
            .into_iter()
            .find(|path| {
                parse_elf(path)
                    .ok()
                    .and_then(|mut parsed| parse_build_id(&mut parsed))
                    .is_some()
            })
            .expect("the Linux analyzer tests require an ELF fixture with a GNU build ID")
    }

    fn write_analyzer_script(path: &Path, source: &str) {
        fs_err::write(path, source).unwrap();
        fs_err::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn assert_requested_tool_failure_is_propagated(debug: bool, strip: bool) {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        let marker = tools.path().join("invoked");
        let failing_program = tools.path().join("failing-analyzer");
        write_analyzer_script(
            &failing_program,
            &format!(
                "#!/bin/sh\nset -eu\nprintf invoked > '{}'\nfor target do :; done\nprintf partial-output > \"$target\"\nexit 9\n",
                marker.display()
            ),
        );

        let mut plan = test_derivation_plan();
        plan.layout.install_dir = runtime.path().join("install").to_string_lossy().into_owned();
        plan.analysis.debug = debug;
        plan.analysis.strip = strip;
        plan.analysis.tools.objcopy = debug.then(|| ExecutablePlan {
            path: failing_program.to_string_lossy().into_owned(),
            requirement: RelationPlan {
                kind: RelationKind::Binary.into(),
                name: "failing-analyzer".to_owned(),
            },
        });
        plan.analysis.tools.strip = strip.then(|| ExecutablePlan {
            path: failing_program.to_string_lossy().into_owned(),
            requirement: RelationPlan {
                kind: RelationKind::Binary.into(),
                name: "failing-analyzer".to_owned(),
            },
        });

        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        let installed = paths.install().guest.join("usr/bin/analyzer-failure-fixture");
        fs_err::create_dir_all(installed.parent().unwrap()).unwrap();
        fs_err::copy(build_id_fixture_path(), &installed).unwrap();
        let original = fs_err::read(&installed).unwrap();
        let original_inode = fs_err::metadata(&installed).unwrap().ino();

        let mut collector = Collector::new(&paths.install().guest);
        collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
        let mut hasher = StoneDigestWriterHasher::new();
        let mut info = collector.path(&installed, &mut hasher).unwrap();
        let mut providers = BTreeSet::new();
        let mut dependencies = BTreeSet::new();
        let mut bucket = BucketMut {
            providers: &mut providers,
            dependencies: &mut dependencies,
            analysis: &plan.analysis,
            install_root: Path::new(&plan.layout.install_dir),
        };

        assert!(
            elf(&mut bucket, &mut info).is_err(),
            "a requested ELF analyzer tool failure must abort package analysis"
        );
        assert!(marker.exists(), "the requested failing analyzer was not invoked");
        assert_eq!(fs_err::read(&installed).unwrap(), original);
        assert_eq!(fs_err::metadata(&installed).unwrap().ino(), original_inode);
        info.verify_unchanged().unwrap();
        collector.seal().unwrap();
    }

    #[test]
    fn strip_mutates_only_a_private_copy_then_commits_transactionally() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let mut plan = test_derivation_plan();
        plan.layout.install_dir = runtime.path().join("install").to_string_lossy().into_owned();
        let tools = tempfile::tempdir().unwrap();
        let observed = tools.path().join("observed-strip-input");
        let strip_program = tools.path().join("strip-fixture");
        write_analyzer_script(
            &strip_program,
            &format!(
                "#!/bin/sh\nset -eu\nfor target do :; done\nprintf '%s' \"$target\" > '{}'\nprintf 'transactional-strip-output' > \"$target\"\n",
                observed.display()
            ),
        );
        plan.analysis.debug = false;
        plan.analysis.strip = true;
        plan.analysis.tools.strip = Some(ExecutablePlan {
            path: strip_program.to_string_lossy().into_owned(),
            requirement: RelationPlan {
                kind: RelationKind::Binary.into(),
                name: "strip-fixture".to_owned(),
            },
        });
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        let installed = paths.install().guest.join("usr/bin/fixture");
        fs_err::create_dir_all(installed.parent().unwrap()).unwrap();
        fs_err::copy(build_id_fixture_path(), &installed).unwrap();
        let original_inode = fs_err::metadata(&installed).unwrap().ino();
        let original_mode = fs_err::metadata(&installed).unwrap().mode();

        let mut collector = Collector::new(&paths.install().guest);
        collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
        let mut hasher = StoneDigestWriterHasher::new();
        let mut info = collector.path(&installed, &mut hasher).unwrap();
        let original_hash = info.file_hash().unwrap();
        let mut providers = BTreeSet::new();
        let mut dependencies = BTreeSet::new();
        let mut bucket = BucketMut {
            providers: &mut providers,
            dependencies: &mut dependencies,
            analysis: &plan.analysis,
            install_root: Path::new(&plan.layout.install_dir),
        };

        let response = elf(&mut bucket, &mut info).unwrap();
        assert!(matches!(response.decision, Decision::IncludeFile));
        assert!(response.publications.is_empty());
        assert_eq!(fs_err::read(&installed).unwrap(), b"transactional-strip-output");
        assert_ne!(fs_err::metadata(&installed).unwrap().ino(), original_inode);
        assert_eq!(fs_err::metadata(&installed).unwrap().mode(), original_mode);
        assert_ne!(info.file_hash().unwrap(), original_hash);
        info.verify_unchanged().unwrap();

        let private_input = PathBuf::from(String::from_utf8(fs_err::read(&observed).unwrap()).unwrap());
        assert_ne!(private_input, installed);
        assert!(private_input.to_string_lossy().contains(".mason-analyzer-"));
        assert!(!private_input.exists(), "private analyzer input was not removed");
        collector.seal().unwrap();
    }

    #[test]
    fn debug_link_mutates_private_copy_and_generated_debug_is_admitted() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let mut plan = test_derivation_plan();
        plan.layout.install_dir = runtime.path().join("install").to_string_lossy().into_owned();
        let tools = tempfile::tempdir().unwrap();
        let observed_split = tools.path().join("observed-split-input");
        let observed_link = tools.path().join("observed-link-input");
        let objcopy_program = tools.path().join("objcopy-fixture");
        write_analyzer_script(
            &objcopy_program,
            &format!(
                "#!/bin/sh\nset -eu\ncase \"$1\" in\n  --only-keep-debug)\n    printf '%s' \"$2\" > '{}'\n    printf 'debug-payload' > \"$3\"\n    ;;\n  --add-gnu-debuglink)\n    printf '%s' \"$3\" > '{}'\n    printf 'debug-link' >> \"$3\"\n    ;;\n  *) exit 64 ;;\nesac\n",
                observed_split.display(),
                observed_link.display()
            ),
        );
        plan.analysis.debug = true;
        plan.analysis.strip = false;
        plan.analysis.tools.objcopy = Some(ExecutablePlan {
            path: objcopy_program.to_string_lossy().into_owned(),
            requirement: RelationPlan {
                kind: RelationKind::Binary.into(),
                name: "objcopy-fixture".to_owned(),
            },
        });
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        let installed = paths.install().guest.join("usr/bin/fixture");
        fs_err::create_dir_all(installed.parent().unwrap()).unwrap();
        fs_err::copy(build_id_fixture_path(), &installed).unwrap();
        let mut expected = fs_err::read(&installed).unwrap();
        expected.extend_from_slice(b"debug-link");

        let mut collector = Collector::new(&paths.install().guest);
        collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
        let mut hasher = StoneDigestWriterHasher::new();
        let mut info = collector.path(&installed, &mut hasher).unwrap();
        let mut providers = BTreeSet::new();
        let mut dependencies = BTreeSet::new();
        let mut bucket = BucketMut {
            providers: &mut providers,
            dependencies: &mut dependencies,
            analysis: &plan.analysis,
            install_root: Path::new(&plan.layout.install_dir),
        };

        let response = elf(&mut bucket, &mut info).unwrap();
        assert!(matches!(response.decision, Decision::IncludeFile));
        assert_eq!(response.publications.len(), 1);
        assert_eq!(fs_err::read(&installed).unwrap(), expected);
        info.verify_unchanged().unwrap();

        let split_input = PathBuf::from(String::from_utf8(fs_err::read(&observed_split).unwrap()).unwrap());
        let link_input = PathBuf::from(String::from_utf8(fs_err::read(&observed_link).unwrap()).unwrap());
        assert_eq!(split_input, link_input);
        assert_ne!(split_input, installed);
        assert!(!split_input.exists(), "private objcopy input was not removed");

        let generated = collector
            .publish_generated(&response.publications, &mut hasher)
            .unwrap();
        assert_eq!(generated.len(), 1);
        assert_eq!(fs_err::read(&generated[0].path).unwrap(), b"debug-payload");
        collector.seal().unwrap();
    }

    #[test]
    fn failed_private_strip_leaves_collected_file_and_witness_unchanged() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let mut plan = test_derivation_plan();
        plan.layout.install_dir = runtime.path().join("install").to_string_lossy().into_owned();
        let tools = tempfile::tempdir().unwrap();
        let observed = tools.path().join("observed-failed-strip-input");
        let strip_program = tools.path().join("failing-strip-fixture");
        write_analyzer_script(
            &strip_program,
            &format!(
                "#!/bin/sh\nset -eu\nfor target do :; done\nprintf '%s' \"$target\" > '{}'\nprintf 'partial-private-output' > \"$target\"\nexit 9\n",
                observed.display()
            ),
        );
        plan.analysis.debug = false;
        plan.analysis.strip = true;
        plan.analysis.tools.strip = Some(ExecutablePlan {
            path: strip_program.to_string_lossy().into_owned(),
            requirement: RelationPlan {
                kind: RelationKind::Binary.into(),
                name: "failing-strip-fixture".to_owned(),
            },
        });
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        let installed = paths.install().guest.join("usr/bin/fixture");
        fs_err::create_dir_all(installed.parent().unwrap()).unwrap();
        fs_err::copy(build_id_fixture_path(), &installed).unwrap();
        let original = fs_err::read(&installed).unwrap();
        let original_inode = fs_err::metadata(&installed).unwrap().ino();

        let mut collector = Collector::new(&paths.install().guest);
        collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
        let mut hasher = StoneDigestWriterHasher::new();
        let mut info = collector.path(&installed, &mut hasher).unwrap();
        let mut providers = BTreeSet::new();
        let mut dependencies = BTreeSet::new();
        let mut bucket = BucketMut {
            providers: &mut providers,
            dependencies: &mut dependencies,
            analysis: &plan.analysis,
            install_root: Path::new(&plan.layout.install_dir),
        };

        assert!(elf(&mut bucket, &mut info).is_err());
        assert_eq!(fs_err::read(&installed).unwrap(), original);
        assert_eq!(fs_err::metadata(&installed).unwrap().ino(), original_inode);
        info.verify_unchanged().unwrap();
        let private_input = PathBuf::from(String::from_utf8(fs_err::read(&observed).unwrap()).unwrap());
        assert_ne!(private_input, installed);
        assert!(!private_input.exists(), "failed analyzer sandbox was not removed");
        collector.seal().unwrap();
    }

    #[test]
    fn executable_dynamic_object_without_interp_is_interpreter_candidate() {
        assert!(is_interpreter_candidate(ET_DYN, 0o100755, false));
    }

    #[test]
    fn ordinary_shared_library_is_not_interpreter_candidate() {
        assert!(!is_interpreter_candidate(ET_DYN, 0o100644, false));
    }

    #[test]
    fn pie_with_own_interp_is_not_interpreter_candidate() {
        assert!(!is_interpreter_candidate(ET_DYN, 0o100755, true));
    }

    #[test]
    fn fixed_address_executable_is_not_interpreter_candidate() {
        assert!(!is_interpreter_candidate(ET_EXEC, 0o100755, false));
    }

    #[test]
    fn onix_private_runpath_qualifies_needed_soname() {
        assert_eq!(
            package_private_rpath_dependency("/usr/lib/onix/stones/rootasrole/4.0.0-5/lib", "libpam.so.0"),
            Some(PathBuf::from("onix/stones/rootasrole/4.0.0-5/lib/libpam.so.0"))
        );
        assert_eq!(package_private_rpath_dependency("/usr/lib", "libpam.so.0"), None);
    }

    #[test]
    fn requested_debug_split_failure_aborts_analysis() {
        assert_requested_tool_failure_is_propagated(true, false);
    }

    #[test]
    fn requested_strip_failure_aborts_analysis() {
        assert_requested_tool_failure_is_propagated(false, true);
    }
}

fn parse_elf(path: &Path) -> Result<elf::ElfStream<AnyEndian, File>, BoxError> {
    let file = File::open(path)?;
    Ok(elf::ElfStream::open_stream(file)?)
}

fn parse_dynamic_section(
    elf: &mut elf::ElfStream<AnyEndian, File>,
    bucket: &mut BucketMut<'_>,
    machine_isa: &str,
    bit_size: Class,
    info: &PathInfo,
    file_name: &str,
) {
    let mut needed_offsets = vec![];
    let mut soname_offset = None;
    let mut rpath_offset = vec![];
    let mut runpath_offset = vec![];

    // i.e `/` `usr` `lib` `libfoo.so.1.2.3`
    let in_root_tree = info.target_path.ancestors().skip(1).count() == 3;

    // Get all dynamic entry offsets into string table
    if let Ok(Some(table)) = elf.dynamic() {
        for entry in table.iter() {
            match entry.d_tag {
                DT_NEEDED => {
                    needed_offsets.push(entry.d_val() as usize);
                }
                DT_SONAME => {
                    soname_offset = Some(entry.d_val() as usize);
                }
                DT_RPATH => {
                    rpath_offset.push(entry.d_val() as usize);
                }
                DT_RUNPATH => {
                    runpath_offset.push(entry.d_val() as usize);
                }
                _ => {}
            }
        }
    }

    // Resolve offsets against string table and add the applicable
    // depends and provides
    if let Ok(Some((_, strtab))) = elf.dynamic_symbol_table() {
        let origin = info.target_path.parent().unwrap().to_string_lossy().to_string();
        let mut rpaths = vec![origin.clone()];

        let root_dir = info
            .path
            .ancestors()
            .find(|p| p.ends_with("usr"))
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| bucket.install_root.to_path_buf());

        for rpath in runpath_offset
            .iter()
            .chain(rpath_offset.iter())
            .filter_map(|v| strtab.get(*v).ok())
        {
            for path in rpath.split(':') {
                let path = clean(path.replace("$ORIGIN", &origin)).to_string_lossy().to_string();
                rpaths.push(path);
            }
        }

        // needed = dependency
        for offset in needed_offsets {
            if let Ok(name) = strtab.get(offset) {
                let rpath_name = rpaths.iter().find_map(|rpath| {
                    if let Some(private_name) = package_private_rpath_dependency(rpath, name) {
                        return Some(private_name);
                    }

                    let local_p = root_dir.join(rpath.trim_start_matches('/')).join(name);
                    let native_p = rpath.to_owned() + "/" + name;
                    let native_path = Path::new(&native_p);
                    if local_p.exists() {
                        Some(
                            Path::new("/")
                                .join(rpath)
                                .join(name)
                                .components()
                                .skip(3)
                                .collect::<PathBuf>(),
                        )
                    } else if native_path.exists() {
                        Some(Path::new(rpath).join(name).components().skip(3).collect::<PathBuf>())
                    } else {
                        None
                    }
                });

                let picked = if let Some(rpath_name) = &rpath_name {
                    &rpath_name.to_string_lossy().to_string()
                } else {
                    name
                };

                bucket.dependencies.insert(Dependency {
                    kind: Kind::SharedLibrary,
                    name: format!("{picked}({machine_isa})"),
                });
            }
        }

        // soname exposed, let's share it
        if file_name.contains(".so") {
            let mut soname = "";

            if let Some(offset) = soname_offset
                && let Ok(val) = strtab.get(offset)
            {
                soname = val;
            }

            if soname.is_empty() {
                soname = file_name;
            }

            // RPATH based soname
            if !in_root_tree {
                let rpathed = info
                    .target_path
                    .parent()
                    .unwrap()
                    .components()
                    .skip(3)
                    .collect::<PathBuf>()
                    .join(soname)
                    .to_string_lossy()
                    .to_string();
                bucket.providers.insert(Provider {
                    kind: Kind::SharedLibrary,
                    name: format!("{rpathed}({machine_isa})"),
                });
            } else {
                bucket.providers.insert(Provider {
                    kind: Kind::SharedLibrary,
                    name: format!("{soname}({machine_isa})"),
                });
            }

            // Do we possibly have an Interpreter? This is a .dynamic library ..
            if soname.starts_with("ld-") && info.target_path.to_str().unwrap_or_default().starts_with("/usr/lib") {
                let interp_paths = if matches!(bit_size, Class::ELF64) {
                    [
                        format!("/usr/lib64/{soname}({machine_isa})"),
                        format!("/lib64/{soname}({machine_isa})"),
                        format!("/lib/{soname}({machine_isa})"),
                        format!("{}({machine_isa})", info.target_path.display()),
                    ]
                } else {
                    [
                        format!("/usr/lib/{soname}({machine_isa})"),
                        format!("/lib32/{soname}({machine_isa})"),
                        format!("/lib/{soname}({machine_isa})"),
                        format!("{}({machine_isa})", info.target_path.display()),
                    ]
                };

                for path in interp_paths {
                    bucket.providers.insert(Provider {
                        kind: Kind::Interpreter,
                        name: path.clone(),
                    });
                    bucket.providers.insert(Provider {
                        kind: Kind::SharedLibrary,
                        name: path,
                    });
                }
            }
        }
    }
}

fn parse_interp_section(elf: &mut elf::ElfStream<AnyEndian, File>, bucket: &mut BucketMut<'_>, machine_isa: &str) {
    let Some(section) = elf.section_header_by_name(".interp").ok().flatten().copied() else {
        return;
    };

    let Ok((data, _)) = elf.section_data(&section) else {
        return;
    };

    if let Some(content) = CStr::from_bytes_until_nul(data).ok().and_then(|s| s.to_str().ok()) {
        bucket.dependencies.insert(Dependency {
            kind: Kind::Interpreter,
            name: format!("{content}({machine_isa})"),
        });
    }
}

fn parse_build_id(elf: &mut elf::ElfStream<AnyEndian, File>) -> Option<String> {
    let section = *elf.section_header_by_name(".note.gnu.build-id").ok()??;
    let notes = elf.section_data_as_notes(&section).ok()?;

    for note in notes {
        if let Note::GnuBuildId(build_id) = note {
            let build_id = hex::encode(build_id.0);
            return Some(build_id);
        }
    }

    None
}

fn pending_debug_destination(info: &PathInfo, bit_size: Class, build_id: &str) -> Result<Option<PathBuf>, BoxError> {
    if build_id.len() < 2 {
        return Ok(None);
    }
    let debug_dir = if matches!(bit_size, Class::ELF64) {
        Path::new("usr/lib/debug/.build-id")
    } else {
        Path::new("usr/lib32/debug/.build-id")
    };
    let destination = debug_dir.join(&build_id[..2]).join(format!("{}.debug", &build_id[2..]));
    let target = Path::new("/").join(&destination);
    Ok((!info.inventory_contains_regular_target(&target)?).then_some(destination))
}

fn split_debug(
    bucket: &BucketMut<'_>,
    info: &PathInfo,
    mutable_input: &Path,
    private_debug_output: &Path,
) -> Result<(), BoxError> {
    let objcopy = &bucket
        .analysis
        .tools
        .objcopy
        .as_ref()
        .expect("validated analysis plan requires objcopy when ELF debug splitting is enabled")
        .path;

    let mut command = analyzer_command(objcopy);
    command
        .arg("--only-keep-debug")
        .arg(mutable_input)
        .arg(private_debug_output)
        .env("LC_ALL", "C");
    checked_output_for(info, command)?;

    let mut command = analyzer_command(objcopy);
    command
        .arg("--add-gnu-debuglink")
        .arg(private_debug_output)
        .arg(mutable_input)
        .env("LC_ALL", "C");
    checked_output_for(info, command)?;

    Ok(())
}

fn strip(bucket: &BucketMut<'_>, info: &PathInfo, mutable_input: &Path) -> Result<(), BoxError> {
    if !bucket.analysis.strip {
        return Ok(());
    }

    let strip = &bucket
        .analysis
        .tools
        .strip
        .as_ref()
        .expect("validated analysis plan requires strip when ELF stripping is enabled")
        .path;
    let is_executable = info
        .path
        .parent()
        .map(|parent| parent.ends_with("bin") || parent.ends_with("sbin"))
        .unwrap_or_default();

    let mut command = analyzer_command(strip);
    command.env("LC_ALL", "C");

    if is_executable {
        command.arg(mutable_input);
    } else {
        command.args(["-g", "--strip-unneeded"]).arg(mutable_input);
    }

    checked_output_for(info, command)?;

    Ok(())
}
