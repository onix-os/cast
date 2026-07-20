use std::{
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
};

use elf::{ElfStream, file::Class};
use stone::StoneDigestWriterHasher;
use stone_recipe::{
    build_policy::AnalyzerKind,
    derivation::{ExecutablePlan, PathRuleKind, RelationKind, RelationPlan},
};

use super::*;
use crate::package::{
    analysis::{Chain, handler::{debug_destination, parse_build_id}},
    collect::{Error as CollectError, ProjectedPathKind},
    test_derivation_plan,
};

#[derive(Debug, PartialEq, Eq)]
struct TreeNode {
    path: PathBuf,
    mode: u32,
    device: u64,
    inode: u64,
    links: u64,
    bytes: Vec<u8>,
}

fn fixture_elf_path() -> PathBuf {
    let mut candidates = vec![PathBuf::from("/bin/sh"), PathBuf::from("/usr/bin/env")];
    candidates.push(std::env::current_exe().unwrap());
    candidates
        .into_iter()
        .find(|path| fixture_debug_destination(path).is_some())
        .expect("the Linux debug-route tests require an ELF fixture with a GNU build ID")
}

fn fixture_debug_destination(path: &Path) -> Option<PathBuf> {
    let file = fs::File::open(path).ok()?;
    let mut stream = ElfStream::open_stream(file).ok()?;
    let class = stream.ehdr.class;
    let build_id = parse_build_id(&mut stream)?;
    debug_destination(class, &build_id)
}

fn install_elf(root: &Path, relative: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::copy(fixture_elf_path(), &path).unwrap();
    path
}

fn source_rule(collector: &mut Collector, relative: &str, package: &str) {
    collector
        .add_rule(&format!("/{relative}"), package, PathRuleKind::Any)
        .unwrap();
}

fn debug_analysis() -> AnalysisPlan {
    let mut plan = test_derivation_plan().analysis;
    plan.debug = true;
    plan.strip = false;
    plan
}

fn objcopy_plan(program: &Path) -> ExecutablePlan {
    ExecutablePlan {
        path: program.to_string_lossy().into_owned(),
        requirement: RelationPlan {
            kind: RelationKind::Binary,
            name: "objcopy-fixture".to_owned(),
        },
    }
}

fn collect_path(collector: &Collector, path: &Path) -> (Vec<PathInfo>, StoneDigestWriterHasher) {
    let mut hasher = StoneDigestWriterHasher::new();
    let info = collector.path(path, &mut hasher).unwrap();
    (vec![info], hasher)
}

fn snapshot_tree(root: &Path) -> Vec<TreeNode> {
    let mut pending = vec![root.to_owned()];
    let mut nodes = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            let metadata = fs::symlink_metadata(&path).unwrap();
            let bytes = if metadata.file_type().is_file() {
                fs::read(&path).unwrap()
            } else if metadata.file_type().is_symlink() {
                fs::read_link(&path).unwrap().as_os_str().as_bytes().to_vec()
            } else {
                Vec::new()
            };
            if metadata.file_type().is_dir() {
                pending.push(path.clone());
            }
            nodes.push(TreeNode {
                path: path.strip_prefix(root).unwrap().to_owned(),
                mode: metadata.mode(),
                device: metadata.dev(),
                inode: metadata.ino(),
                links: metadata.nlink(),
                bytes,
            });
        }
    }
    nodes.sort_by(|left, right| left.path.cmp(&right.path));
    nodes
}

fn assert_missing_route(error: &BoxError, expected: &Path) {
    assert!(matches!(
        error.downcast_ref::<CollectError>(),
        Some(CollectError::NoMatchingRule { path }) if path == expected
    ));
}

#[test]
fn missing_elf_debug_route_fails_before_chain_without_objcopy_or_tree_mutation() {
    let root = tempfile::tempdir().unwrap();
    let source = install_elf(root.path(), "usr/bin/fixture");
    let destination = fixture_debug_destination(&source).unwrap();
    let tools = tempfile::tempdir().unwrap();
    let marker = tools.path().join("objcopy-was-invoked");
    let program = tools.path().join("objcopy-fixture");
    fs::write(
        &program,
        format!("#!/bin/sh\nprintf invoked > '{}'\nexit 70\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let mut plan = debug_analysis();
    plan.tools.objcopy = Some(objcopy_plan(&program));
    let mut collector = Collector::new(root.path());
    source_rule(&mut collector, "usr/bin/fixture", "runtime");
    let (paths, _) = collect_path(&collector, &source);
    let before = snapshot_tree(root.path());
    let source_before = fs::metadata(&source).unwrap();

    let error = preflight_elf_debug_routes(&plan, &collector, &paths).unwrap_err();

    assert_missing_route(&error, &root.path().join(destination));
    assert!(!marker.exists());
    assert_eq!(snapshot_tree(root.path()), before);
    let source_after = fs::metadata(&source).unwrap();
    assert_eq!(source_after.ino(), source_before.ino());
    assert_eq!(source_after.mode(), source_before.mode());
}

#[test]
fn missing_elf_debug_route_leaves_every_path_verified_and_inventory_sealable() {
    let root = tempfile::tempdir().unwrap();
    let source = install_elf(root.path(), "usr/bin/fixture");
    let mut collector = Collector::new(root.path());
    source_rule(&mut collector, "usr/bin/fixture", "runtime");
    let (paths, _) = collect_path(&collector, &source);

    assert!(preflight_elf_debug_routes(&debug_analysis(), &collector, &paths).is_err());
    for info in &paths {
        info.verify_unchanged().unwrap();
    }
    collector.seal().unwrap();
}

#[test]
fn elf_debug_preflight_uses_witnessed_bytes_instead_of_a_replaced_path() {
    let root = tempfile::tempdir().unwrap();
    let source = install_elf(root.path(), "usr/bin/fixture");
    let displaced = root.path().join("original-elf");
    let mut collector = Collector::new(root.path());
    source_rule(&mut collector, "usr/bin/fixture", "runtime");
    let (paths, _) = collect_path(&collector, &source);
    fs::rename(&source, &displaced).unwrap();
    fs::copy(fixture_elf_path(), &source).unwrap();

    let error = preflight_elf_debug_routes(&debug_analysis(), &collector, &paths).unwrap_err();

    assert!(error.downcast_ref::<CollectError>().is_some());
    assert!(!root.path().join("usr/lib/debug").exists());
    assert_eq!(fs::read(&displaced).unwrap(), fs::read(fixture_elf_path()).unwrap());

    let between_root = tempfile::tempdir().unwrap();
    let between_source = install_elf(between_root.path(), "usr/bin/fixture");
    let between_destination = fixture_debug_destination(&between_source).unwrap();
    let between_displaced = between_root.path().join("original-elf");
    let tools = tempfile::tempdir().unwrap();
    let marker = tools.path().join("objcopy-was-invoked");
    let program = tools.path().join("objcopy-fixture");
    fs::write(
        &program,
        format!("#!/bin/sh\nprintf invoked > '{}'\nexit 70\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let mut plan = debug_analysis();
    plan.tools.objcopy = Some(objcopy_plan(&program));
    let mut between = Collector::new(between_root.path());
    source_rule(&mut between, "usr/bin/fixture", "runtime");
    between
        .add_rule(
            &format!("/{}", between_destination.display()),
            "debug",
            PathRuleKind::Any,
        )
        .unwrap();
    let (between_paths, mut hasher) = collect_path(&between, &between_source);
    preflight_elf_debug_routes(&plan, &between, &between_paths).unwrap();
    fs::rename(&between_source, &between_displaced).unwrap();
    fs::copy(fixture_elf_path(), &between_source).unwrap();
    let replacement_inode = fs::metadata(&between_source).unwrap().ino();

    let mut chain = Chain::new(between_root.path(), &plan, &between, &mut hasher);
    let error = chain.process(between_paths).unwrap_err();

    assert!(error.downcast_ref::<CollectError>().is_some());
    assert!(!marker.exists());
    assert_eq!(fs::metadata(&between_source).unwrap().ino(), replacement_inode);
    assert_eq!(fs::read(&between_displaced).unwrap(), fs::read(fixture_elf_path()).unwrap());
}

#[test]
fn elf32_and_elf64_build_ids_have_class_specific_debug_destinations() {
    assert_eq!(debug_destination(Class::ELF64, ""), None);
    assert_eq!(debug_destination(Class::ELF32, "a"), None);
    assert_eq!(
        debug_destination(Class::ELF64, "ab").unwrap(),
        PathBuf::from("usr/lib/debug/.build-id/ab/.debug")
    );
    assert_eq!(
        debug_destination(Class::ELF32, "abcdef").unwrap(),
        PathBuf::from("usr/lib32/debug/.build-id/ab/cdef.debug")
    );
}

#[test]
fn elf_debug_route_uses_non_executable_mode_and_reverse_rule_precedence() {
    let rejected_root = tempfile::tempdir().unwrap();
    let rejected_source = install_elf(rejected_root.path(), "usr/bin/fixture");
    let destination = fixture_debug_destination(&rejected_source).unwrap();
    let mut rejected = Collector::new(rejected_root.path());
    source_rule(&mut rejected, "usr/bin/fixture", "runtime");
    rejected
        .add_rule(
            &format!("/{}", destination.display()),
            "executable-only",
            PathRuleKind::Executable,
        )
        .unwrap();
    let (paths, _) = collect_path(&rejected, &rejected_source);
    assert!(preflight_elf_debug_routes(&debug_analysis(), &rejected, &paths).is_err());
    rejected.seal().unwrap();

    let accepted_root = tempfile::tempdir().unwrap();
    let accepted_source = install_elf(accepted_root.path(), "usr/bin/fixture");
    let destination = fixture_debug_destination(&accepted_source).unwrap();
    let mut accepted = Collector::new(accepted_root.path());
    source_rule(&mut accepted, "usr/bin/fixture", "runtime");
    let pattern = format!("/{}", destination.display());
    accepted.add_rule(&pattern, "lower", PathRuleKind::Any).unwrap();
    accepted.add_rule(&pattern, "higher", PathRuleKind::Any).unwrap();
    let (paths, _) = collect_path(&accepted, &accepted_source);
    preflight_elf_debug_routes(&debug_analysis(), &accepted, &paths).unwrap();
    assert_eq!(
        accepted
            .projected_package_for(&destination, ProjectedPathKind::Regular { mode: 0o644 })
            .unwrap()
            .as_ref(),
        "higher"
    );
    accepted.seal().unwrap();
}

#[test]
fn elf_debug_preflight_respects_switch_reachability_and_non_elf_inputs() {
    let root = tempfile::tempdir().unwrap();
    let source = install_elf(root.path(), "usr/bin/fixture");
    let mut collector = Collector::new(root.path());
    source_rule(&mut collector, "usr/bin/fixture", "runtime");
    let (paths, _) = collect_path(&collector, &source);
    let mut disabled = debug_analysis();
    disabled.debug = false;
    preflight_elf_debug_routes(&disabled, &collector, &paths).unwrap();
    let mut missing = debug_analysis();
    missing.handlers = vec![AnalyzerKind::IgnoreBlocked, AnalyzerKind::IncludeAny];
    preflight_elf_debug_routes(&missing, &collector, &paths).unwrap();
    collector.seal().unwrap();

    let non_elf_root = tempfile::tempdir().unwrap();
    let text = non_elf_root.path().join("usr/bin/text");
    fs::create_dir_all(text.parent().unwrap()).unwrap();
    fs::write(&text, b"not an ELF").unwrap();
    let mut non_elf = Collector::new(non_elf_root.path());
    source_rule(&mut non_elf, "usr/bin/text", "runtime");
    let (paths, _) = collect_path(&non_elf, &text);
    preflight_elf_debug_routes(&debug_analysis(), &non_elf, &paths).unwrap();
    non_elf.seal().unwrap();

    let blocked_root = tempfile::tempdir().unwrap();
    let blocked_source = install_elf(blocked_root.path(), "opt/fixture");
    let mut blocked = Collector::new(blocked_root.path());
    source_rule(&mut blocked, "opt/fixture", "runtime");
    let (paths, _) = collect_path(&blocked, &blocked_source);
    assert!(!elf_handler_reachable(&debug_analysis(), &paths[0]));
    preflight_elf_debug_routes(&debug_analysis(), &blocked, &paths).unwrap();
    blocked.seal().unwrap();

    let man_root = tempfile::tempdir().unwrap();
    let man_source = install_elf(man_root.path(), "usr/share/man/man1/fixture.1");
    let mut man = Collector::new(man_root.path());
    source_rule(&mut man, "usr/share/man/man1/fixture.1", "runtime");
    let (paths, _) = collect_path(&man, &man_source);
    let mut compress_first = debug_analysis();
    compress_first.handlers = vec![AnalyzerKind::CompressMan, AnalyzerKind::Elf, AnalyzerKind::IncludeAny];
    assert!(!elf_handler_reachable(&compress_first, &paths[0]));
    preflight_elf_debug_routes(&compress_first, &man, &paths).unwrap();
    let mut elf_first = debug_analysis();
    elf_first.handlers = vec![AnalyzerKind::Elf, AnalyzerKind::CompressMan, AnalyzerKind::IncludeAny];
    assert!(elf_handler_reachable(&elf_first, &paths[0]));
    assert!(preflight_elf_debug_routes(&elf_first, &man, &paths).is_err());
    man.seal().unwrap();
}

#[test]
fn existing_debug_destination_accepts_regular_and_rejects_nonregular_before_effects() {
    let regular_root = tempfile::tempdir().unwrap();
    let regular_source = install_elf(regular_root.path(), "usr/bin/fixture");
    let regular_destination = fixture_debug_destination(&regular_source).unwrap();
    let regular_destination_path = regular_root.path().join(regular_destination);
    fs::create_dir_all(regular_destination_path.parent().unwrap()).unwrap();
    fs::write(&regular_destination_path, b"existing-debug").unwrap();
    let regular_source_before = fs::metadata(&regular_source).unwrap();
    let mut regular = Collector::new(regular_root.path());
    source_rule(&mut regular, "usr/bin/fixture", "runtime");
    let (regular_paths, _) = collect_path(&regular, &regular_source);

    preflight_elf_debug_routes(&debug_analysis(), &regular, &regular_paths).unwrap();

    assert_eq!(fs::metadata(&regular_source).unwrap().ino(), regular_source_before.ino());
    assert_eq!(fs::read(&regular_destination_path).unwrap(), b"existing-debug");
    regular.seal().unwrap();

    let root = tempfile::tempdir().unwrap();
    let source = install_elf(root.path(), "usr/bin/fixture");
    let destination = fixture_debug_destination(&source).unwrap();
    let destination_path = root.path().join(&destination);
    fs::create_dir_all(destination_path.parent().unwrap()).unwrap();
    symlink("foreign-debug", &destination_path).unwrap();
    let source_before = fs::metadata(&source).unwrap();
    let bytes_before = fs::read(&source).unwrap();
    let mut collector = Collector::new(root.path());
    source_rule(&mut collector, "usr/bin/fixture", "runtime");
    collector
        .add_rule(
            &format!("/{}", destination.display()),
            "foreign-link",
            PathRuleKind::Symlink,
        )
        .unwrap();
    let (paths, _) = collect_path(&collector, &source);

    let error = preflight_elf_debug_routes(&debug_analysis(), &collector, &paths).unwrap_err();

    assert!(matches!(
        error.downcast_ref::<CollectError>(),
        Some(CollectError::ExistingAdmission { path }) if path == &Path::new("/").join(&destination)
    ));
    assert_eq!(fs::read(&source).unwrap(), bytes_before);
    assert_eq!(fs::metadata(&source).unwrap().ino(), source_before.ino());
    assert_eq!(fs::read_link(&destination_path).unwrap(), Path::new("foreign-debug"));
    for info in &paths {
        info.verify_unchanged().unwrap();
    }
    collector.seal().unwrap();

    let ancestor_root = tempfile::tempdir().unwrap();
    let ancestor_source = install_elf(ancestor_root.path(), "usr/bin/fixture");
    let ancestor_destination = fixture_debug_destination(&ancestor_source).unwrap();
    let blocked_ancestor = ancestor_destination.components().take(3).collect::<PathBuf>();
    let blocked_ancestor_path = ancestor_root.path().join(&blocked_ancestor);
    fs::create_dir_all(blocked_ancestor_path.parent().unwrap()).unwrap();
    symlink("foreign-debug-tree", &blocked_ancestor_path).unwrap();
    let ancestor_source_before = fs::metadata(&ancestor_source).unwrap();
    let mut ancestor = Collector::new(ancestor_root.path());
    source_rule(&mut ancestor, "usr/bin/fixture", "runtime");
    let (ancestor_paths, _) = collect_path(&ancestor, &ancestor_source);

    let error = preflight_elf_debug_routes(&debug_analysis(), &ancestor, &ancestor_paths).unwrap_err();

    assert!(matches!(
        error.downcast_ref::<CollectError>(),
        Some(CollectError::ExistingAdmission { path })
            if path == &Path::new("/").join(&ancestor_destination)
    ));
    assert_eq!(fs::metadata(&ancestor_source).unwrap().ino(), ancestor_source_before.ino());
    assert_eq!(fs::read_link(&blocked_ancestor_path).unwrap(), Path::new("foreign-debug-tree"));
    ancestor.seal().unwrap();
}

#[test]
fn routed_debug_output_runs_objcopy_only_after_global_preflight() {
    let root = tempfile::tempdir().unwrap();
    let source = install_elf(root.path(), "usr/bin/fixture");
    let source_inode = fs::metadata(&source).unwrap().ino();
    let destination = fixture_debug_destination(&source).unwrap();
    let tools = tempfile::tempdir().unwrap();
    let marker = tools.path().join("objcopy-invocations");
    let program = tools.path().join("objcopy-fixture");
    fs::write(
        &program,
        format!(
            "#!/bin/sh\nset -eu\nprintf x >> '{}'\ncase \"$1\" in\n  --only-keep-debug) printf debug-payload > \"$3\" ;;\n  --add-gnu-debuglink) printf debug-link >> \"$3\" ;;\n  *) exit 64 ;;\nesac\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

    let mut plan = debug_analysis();
    plan.tools.objcopy = Some(objcopy_plan(&program));
    let mut collector = Collector::new(root.path());
    collector.add_rule("*", "runtime", PathRuleKind::Any).unwrap();
    collector
        .add_rule(&format!("/{}", destination.display()), "debug", PathRuleKind::Any)
        .unwrap();
    let mut hasher = StoneDigestWriterHasher::new();
    let paths = collector.enumerate_paths(None, &mut hasher).unwrap();

    preflight_elf_debug_routes(&plan, &collector, &paths).unwrap();
    assert!(!marker.exists());
    assert_eq!(fs::metadata(&source).unwrap().ino(), source_inode);

    let mut chain = Chain::new(root.path(), &plan, &collector, &mut hasher);
    let _sealed = chain.process(paths).unwrap();
    assert_eq!(fs::read(&marker).unwrap(), b"xx");
    assert_ne!(fs::metadata(&source).unwrap().ino(), source_inode);
    assert_eq!(fs::read(root.path().join(destination)).unwrap(), b"debug-payload");
    assert!(
        chain.buckets["runtime"]
            .paths
            .iter()
            .any(|info| info.target_path == Path::new("/usr/bin/fixture"))
    );
    assert_eq!(chain.buckets["debug"].paths.len(), 1);
}
