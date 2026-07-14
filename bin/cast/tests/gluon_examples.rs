use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsStr,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

use tempfile::TempDir;

const RECIPE_FILE_NAME: &str = "stone.glu";

#[test]
fn every_gluon_package_example_passes_the_public_cast_cli() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve the repository root from bin/cast");
    let examples = repository.join("docs/examples/gluon/packages");
    let recipes = discover_recipes(&examples);
    let isolation = Isolation::new();
    let authored = snapshot_tree(&examples);

    for recipe in recipes {
        let package = recipe
            .parent()
            .expect("a discovered recipe always has a package directory");
        assert_all_gluon_modules_are_reachable(package);

        let checked = run_cast(&isolation, "check", &recipe);
        assert_success(&recipe, "recipe check", &checked);
        let check_stdout = utf8_stdout(&recipe, "recipe check", &checked);
        assert!(
            check_stdout.contains(" is valid ("),
            "{}: `cast recipe check` succeeded without its validation confirmation:\n{check_stdout}",
            recipe.display()
        );
        assert_tree_unchanged(&examples, &authored, &recipe, "recipe check");

        let first_eval = run_cast(&isolation, "eval", &recipe);
        assert_success(&recipe, "first recipe eval", &first_eval);
        assert_tree_unchanged(&examples, &authored, &recipe, "first recipe eval");

        let second_eval = run_cast(&isolation, "eval", &recipe);
        assert_success(&recipe, "second recipe eval", &second_eval);
        assert_tree_unchanged(&examples, &authored, &recipe, "second recipe eval");

        let first_stdout = utf8_stdout(&recipe, "first recipe eval", &first_eval);
        let second_stdout = utf8_stdout(&recipe, "second recipe eval", &second_eval);
        assert!(
            first_stdout.contains("package-v3-evaluation {"),
            "{}: `cast recipe eval` succeeded without a package-v3 declaration:\n{first_stdout}",
            recipe.display()
        );
        assert_eq!(
            first_stdout,
            second_stdout,
            "{}: repeated `cast recipe eval` calls produced different declarations",
            recipe.display()
        );
        assert_eq!(
            first_eval.stderr,
            second_eval.stderr,
            "{}: repeated `cast recipe eval` calls produced different diagnostics",
            recipe.display()
        );
    }
}

fn discover_recipes(examples: &Path) -> Vec<PathBuf> {
    let entries = sorted_directory_entries(examples);
    assert!(
        !entries.is_empty(),
        "no Gluon package examples found under {}; add at least one <package>/{RECIPE_FILE_NAME}",
        examples.display()
    );

    let mut names = BTreeSet::new();
    let mut canonical_roots = BTreeSet::new();
    let mut recipes = Vec::new();

    for entry in entries {
        let file_type = entry
            .file_type()
            .unwrap_or_else(|error| panic!("inspect example entry {}: {error}", entry.path().display()));
        assert!(
            file_type.is_dir() && !file_type.is_symlink(),
            "unexpected entry directly under {}: {} must be a real package directory",
            examples.display(),
            entry.path().display()
        );

        let name = entry.file_name().into_string().unwrap_or_else(|name| {
            panic!(
                "example package directory is not valid UTF-8: {}",
                PathBuf::from(name).display()
            )
        });
        assert!(
            names.insert(name.clone()),
            "duplicate Gluon example package directory {name:?} under {}",
            examples.display()
        );

        let package = entry.path();
        let roots = gluon_files_beneath(&package)
            .into_iter()
            .filter(|path| path.file_name() == Some(OsStr::new(RECIPE_FILE_NAME)))
            .collect::<Vec<_>>();
        assert_eq!(
            roots.len(),
            1,
            "{} must contain exactly one {RECIPE_FILE_NAME} root, found: {}",
            package.display(),
            display_paths(&roots)
        );

        let expected = package.join(RECIPE_FILE_NAME);
        assert_eq!(
            roots[0],
            expected,
            "unexpected nested recipe root {}; package roots must be exactly <package>/{RECIPE_FILE_NAME}",
            roots[0].display()
        );
        let metadata = fs::symlink_metadata(&expected)
            .unwrap_or_else(|error| panic!("inspect recipe root {}: {error}", expected.display()));
        assert!(
            metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
            "recipe root {} must be a regular, non-symlink file",
            expected.display()
        );

        let canonical = expected
            .canonicalize()
            .unwrap_or_else(|error| panic!("resolve recipe root {}: {error}", expected.display()));
        assert!(
            canonical_roots.insert(canonical),
            "duplicate canonical recipe root discovered at {}",
            expected.display()
        );
        recipes.push(expected);
    }

    assert!(
        !recipes.is_empty(),
        "no <package>/{RECIPE_FILE_NAME} roots discovered under {}",
        examples.display()
    );
    recipes
}

fn assert_all_gluon_modules_are_reachable(package: &Path) {
    let all_modules = gluon_files_beneath(package)
        .into_iter()
        .map(|path| {
            path.strip_prefix(package)
                .expect("walked package module remains beneath its package")
                .to_path_buf()
        })
        .collect::<BTreeSet<_>>();
    let mut reachable = BTreeSet::new();
    let mut pending = VecDeque::from([PathBuf::from(RECIPE_FILE_NAME)]);

    while let Some(module) = pending.pop_front() {
        if !reachable.insert(module.clone()) {
            continue;
        }
        assert!(
            all_modules.contains(&module),
            "{} imports missing support module {}",
            package.display(),
            module.display()
        );

        let path = package.join(&module);
        let source =
            fs::read_to_string(&path).unwrap_or_else(|error| panic!("read Gluon module {}: {error}", path.display()));
        for imported in quoted_gluon_imports(&source) {
            let imported = Path::new(&imported);
            assert!(
                imported.starts_with("./"),
                "{} uses quoted Gluon import {imported:?}; support modules must use package-local ./ paths",
                path.display()
            );
            assert!(
                imported
                    .components()
                    .all(|component| { matches!(component, Component::CurDir | Component::Normal(_)) }),
                "{} uses support import {imported:?} that escapes its package directory",
                path.display()
            );

            let relative = module
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(imported.strip_prefix("./").expect("checked ./ import"));
            pending.push_back(relative);
        }
    }

    let orphaned = all_modules.difference(&reachable).cloned().collect::<Vec<_>>();
    assert!(
        orphaned.is_empty(),
        "{} contains Gluon support modules unreachable from {RECIPE_FILE_NAME}: {}",
        package.display(),
        display_paths(&orphaned)
    );
}

fn quoted_gluon_imports(source: &str) -> Vec<String> {
    let mut remaining = source;
    let mut imports = Vec::new();

    while let Some(offset) = remaining.find("import!") {
        remaining = &remaining[offset + "import!".len()..];
        let candidate = remaining.trim_start();
        let Some(quoted) = candidate.strip_prefix('"') else {
            continue;
        };
        let Some(end) = quoted.find('"') else {
            continue;
        };
        let import = &quoted[..end];
        if import.ends_with(".glu") {
            imports.push(import.to_owned());
        }
    }

    imports
}

fn gluon_files_beneath(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(directory) = pending.pop() {
        for entry in sorted_directory_entries(&directory) {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .unwrap_or_else(|error| panic!("inspect example path {}: {error}", path.display()));
            assert!(
                !file_type.is_symlink(),
                "example tree must not contain symlinks: {}",
                path.display()
            );
            if file_type.is_dir() {
                pending.push(path);
            } else {
                assert!(
                    file_type.is_file(),
                    "example tree must contain only directories and regular files: {}",
                    path.display()
                );
                if path.extension() == Some(OsStr::new("glu")) {
                    files.push(path);
                }
            }
        }
    }

    files.sort();
    files
}

fn sorted_directory_entries(directory: &Path) -> Vec<fs::DirEntry> {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read example directory {}: {error}", directory.display()))
        .map(|entry| entry.unwrap_or_else(|error| panic!("read an entry beneath {}: {error}", directory.display())))
        .collect::<Vec<_>>();
    entries.sort_by_key(fs::DirEntry::file_name);
    entries
}

struct Isolation {
    _temporary: TempDir,
    root: PathBuf,
}

impl Isolation {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("create isolated Cast example environment");
        let root = temporary.path().to_path_buf();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("make isolated Cast example root owner-private");
        for directory in [
            "home",
            "xdg/cache",
            "xdg/config",
            "xdg/data",
            "xdg/runtime",
            "xdg/state",
            "package-cache",
            "build-cache",
            "config",
            "data",
            "resolver",
            "system-root",
        ] {
            let path = root.join(directory);
            fs::create_dir_all(&path)
                .unwrap_or_else(|error| panic!("create isolated Cast directory {}: {error}", path.display()));
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .unwrap_or_else(|error| panic!("make isolated Cast directory {} private: {error}", path.display()));
        }
        Self {
            _temporary: temporary,
            root,
        }
    }
}

fn run_cast(isolation: &Isolation, operation: &str, recipe: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cast"))
        .arg("--directory")
        .arg(isolation.root.join("system-root"))
        .arg("--package-cache-dir")
        .arg(isolation.root.join("package-cache"))
        .arg("--build-cache-dir")
        .arg(isolation.root.join("build-cache"))
        .arg("--config-dir")
        .arg(isolation.root.join("config"))
        .arg("--data-dir")
        .arg(isolation.root.join("data"))
        .arg("--resolver-root")
        .arg(isolation.root.join("resolver"))
        .arg("recipe")
        .arg(operation)
        .arg(recipe)
        .env("HOME", isolation.root.join("home"))
        .env("XDG_CACHE_HOME", isolation.root.join("xdg/cache"))
        .env("XDG_CONFIG_HOME", isolation.root.join("xdg/config"))
        .env("XDG_DATA_HOME", isolation.root.join("xdg/data"))
        .env("XDG_RUNTIME_DIR", isolation.root.join("xdg/runtime"))
        .env("XDG_STATE_HOME", isolation.root.join("xdg/state"))
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap_or_else(|error| panic!("execute `cast recipe {operation}` for {}: {error}", recipe.display()))
}

fn assert_success(recipe: &Path, operation: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{}: `{operation}` failed with {}\nstdout:\n{}\nstderr:\n{}",
        recipe.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn utf8_stdout<'a>(recipe: &Path, operation: &str, output: &'a Output) -> &'a str {
    std::str::from_utf8(&output.stdout)
        .unwrap_or_else(|error| panic!("{}: `{operation}` emitted non-UTF-8 stdout: {error}", recipe.display()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TreeEntry {
    kind: EntryKind,
    mode: u32,
    contents: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    let mut snapshot = BTreeMap::new();
    snapshot_path(root, root, &mut snapshot);
    snapshot
}

fn snapshot_path(root: &Path, path: &Path, snapshot: &mut BTreeMap<PathBuf, TreeEntry>) {
    let metadata =
        fs::symlink_metadata(path).unwrap_or_else(|error| panic!("inspect example path {}: {error}", path.display()));
    let relative = path
        .strip_prefix(root)
        .expect("snapshotted path remains beneath package root")
        .to_path_buf();
    let mode = metadata.permissions().mode();

    if metadata.file_type().is_dir() {
        snapshot.insert(
            relative,
            TreeEntry {
                kind: EntryKind::Directory,
                mode,
                contents: Vec::new(),
            },
        );
        for entry in sorted_directory_entries(path) {
            snapshot_path(root, &entry.path(), snapshot);
        }
    } else if metadata.file_type().is_file() {
        let contents = fs::read(path).unwrap_or_else(|error| panic!("read example file {}: {error}", path.display()));
        snapshot.insert(
            relative,
            TreeEntry {
                kind: EntryKind::File,
                mode,
                contents,
            },
        );
    } else {
        panic!(
            "example tree must contain only directories and regular files: {}",
            path.display()
        );
    }
}

fn assert_tree_unchanged(examples: &Path, before: &BTreeMap<PathBuf, TreeEntry>, recipe: &Path, operation: &str) {
    let after = snapshot_tree(examples);
    if before == &after {
        return;
    }

    let changed = before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .map(|path| {
            if path.as_os_str().is_empty() {
                ".".to_owned()
            } else {
                path.display().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    panic!(
        "{}: `cast {operation}` mutated the authored tree under {}; changed entries: {changed}",
        recipe.display(),
        examples.display()
    );
}

fn display_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        "<none>".to_owned()
    } else {
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}
