// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{fs, path::Path};

use tempfile::TempDir;

#[path = "../src/semantic_fingerprint.rs"]
mod semantic_fingerprint;

use semantic_fingerprint::{ExplicitInput, calculate};

const BASE_FILES: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"bin/boulder\", \"crates/stone\"]\n",
    ),
    ("Cargo.lock", "version = 4\n"),
    ("flake.nix", "{ outputs = _: {}; }\n"),
    ("flake.lock", "{}\n"),
    ("Makefile", "build:\n\tcargo build\n"),
    ("bin/boulder/Cargo.toml", "[package]\nname = \"boulder\"\n"),
    ("bin/boulder/src/main.rs", "fn main() {}\n"),
    ("bin/boulder/data/policy/default.glu", "{ target = \"native\" }\n"),
    ("crates/stone/Cargo.toml", "[package]\nname = \"stone\"\n"),
    ("crates/stone/build.rs", "fn main() {}\n"),
    ("crates/stone/src/lib.rs", "pub fn read() {}\n"),
    ("crates/stone/gluon/stone.glu", "{ format = 1 }\n"),
];

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn fixture(files: impl IntoIterator<Item = (&'static str, &'static str)>) -> TempDir {
    let temporary = tempfile::tempdir().unwrap();
    for (path, contents) in files {
        write(temporary.path(), path, contents);
    }
    temporary
}

fn base_fixture() -> TempDir {
    fixture(BASE_FILES.iter().copied())
}

fn context() -> Vec<ExplicitInput> {
    vec![
        ("env.PROFILE".to_owned(), b"release".to_vec()),
        ("env.TARGET".to_owned(), b"x86_64-unknown-linux-gnu".to_vec()),
        ("toolchain.rustc-vV".to_owned(), b"rustc fixture".to_vec()),
    ]
}

fn value(root: &Path) -> String {
    calculate(root, context()).unwrap().value().to_owned()
}

#[test]
fn build_exports_a_versioned_sha256_value() {
    let fingerprint = tools_buildinfo::get_semantic_fingerprint();
    let digest = fingerprint.strip_prefix("sha256:").unwrap();
    assert_eq!(digest.len(), 64);
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

#[test]
fn discovery_is_checkout_and_creation_order_independent() {
    let forward = fixture(BASE_FILES.iter().copied());
    let reverse = fixture(BASE_FILES.iter().rev().copied());

    let first = calculate(forward.path(), context()).unwrap();
    let second = calculate(reverse.path(), context().into_iter().rev()).unwrap();

    assert_eq!(first.value(), second.value());
    assert_eq!(first.relative_paths(), second.relative_paths());
    assert!(
        first
            .watched_paths()
            .iter()
            .any(|path| path.ends_with("crates/stone/src/lib.rs"))
    );
    assert_eq!(
        first.relative_paths(),
        &[
            "Cargo.lock",
            "Cargo.toml",
            "Makefile",
            "bin/boulder/Cargo.toml",
            "bin/boulder/data/policy/default.glu",
            "bin/boulder/src/main.rs",
            "crates/stone/Cargo.toml",
            "crates/stone/build.rs",
            "crates/stone/gluon/stone.glu",
            "crates/stone/src/lib.rs",
            "flake.lock",
            "flake.nix",
        ]
    );
}

#[test]
fn source_archives_and_dirty_trees_are_identified_by_contents() {
    let repository = base_fixture();
    let original = value(repository.path());

    // No Git repository or status string participates in this calculation.
    write(
        repository.path(),
        "crates/stone/src/lib.rs",
        "pub fn read() { changed(); }\n",
    );
    assert_ne!(value(repository.path()), original);

    write(repository.path(), "crates/stone/src/extra.rs", "pub fn extra() {}\n");
    let with_extra_source = value(repository.path());
    fs::remove_file(repository.path().join("crates/stone/src/extra.rs")).unwrap();
    assert_ne!(value(repository.path()), with_extra_source);
}

#[test]
fn lock_manifests_policy_and_toolchain_files_are_all_semantic() {
    for path in [
        "Cargo.lock",
        "Cargo.toml",
        "bin/boulder/Cargo.toml",
        "crates/stone/Cargo.toml",
        "bin/boulder/data/policy/default.glu",
        "flake.nix",
        "flake.lock",
        "Makefile",
    ] {
        let repository = base_fixture();
        let original = value(repository.path());
        write(repository.path(), path, "changed semantic input\n");
        assert_ne!(value(repository.path()), original, "{path} must affect the fingerprint");
    }
}

#[test]
fn docs_tests_examples_generated_outputs_and_timestamps_are_excluded() {
    let repository = base_fixture();
    let original = value(repository.path());

    for path in [
        "README.md",
        "docs/design.md",
        "tests/fixtures/package.stone",
        "bin/boulder/README.md",
        "bin/boulder/tests/cli.rs",
        "crates/stone/examples/read.rs",
        "crates/stone/benches/read.rs",
        "target/debug/boulder",
        ".direnv/profile",
        ".git/index",
    ] {
        write(repository.path(), path, "not a production input\n");
    }
    assert_eq!(value(repository.path()), original);

    // Rewriting identical bytes changes metadata but not semantic identity.
    write(repository.path(), "crates/stone/src/lib.rs", "pub fn read() {}\n");
    assert_eq!(value(repository.path()), original);
}

#[test]
fn target_profile_features_compiler_and_flags_are_length_prefixed_inputs() {
    let repository = base_fixture();
    let original = value(repository.path());

    for (name, changed) in [
        ("env.PROFILE", b"debug".as_slice()),
        ("env.TARGET", b"aarch64-unknown-linux-gnu".as_slice()),
        ("env.CARGO_FEATURE_EXPERIMENTAL", b"1".as_slice()),
        ("env.CARGO_CFG_TARGET_FEATURE", b"avx2".as_slice()),
        ("env.CARGO_ENCODED_RUSTFLAGS", b"-Ctarget-cpu=native".as_slice()),
        ("toolchain.rustc-vV", b"rustc other".as_slice()),
    ] {
        let mut changed_context = context();
        if let Some((_, value)) = changed_context.iter_mut().find(|(key, _)| key == name) {
            *value = changed.to_vec();
        } else {
            changed_context.push((name.to_owned(), changed.to_vec()));
        }
        let changed_value = calculate(repository.path(), changed_context).unwrap();
        assert_ne!(changed_value.value(), original, "{name} must affect the fingerprint");
    }

    // Length framing makes the two otherwise ambiguous byte streams distinct.
    let left = calculate(repository.path(), [("a".to_owned(), b"bc".to_vec())]).unwrap();
    let right = calculate(repository.path(), [("ab".to_owned(), b"c".to_vec())]).unwrap();
    assert_ne!(left.value(), right.value());
}
