//! Harness-free delegated contentful execution fixture.
//!
//! libtest owns a worker pool and therefore cannot supervise the production
//! `clone3` boundary, which deliberately audits `/proc/self/task` immediately
//! before creating a child. This target has no test harness and calls the
//! feature-gated Mason test-support entry point directly from its sole task.

use std::{
    env, fs,
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

const PROOF_FILE_NAME: &str = "fixtures-ci-proof.json";
const PROOF_TEMP_FILE_NAME: &str = ".fixtures-ci-proof.json.tmp";

fn main() {
    match env::var("CAST_DELEGATED_FIXTURE_RUNNER") {
        Ok(value) if value == "1" => {
            mason::delegated_fixture_test_support::run();
            write_required_matrix_proof_if_requested();
        }
        Err(env::VarError::NotPresent) => {
            eprintln!("delegated execution fixture is runner-only; use `make delegated-execution-fixtures`");
        }
        Ok(value) => panic!("CAST_DELEGATED_FIXTURE_RUNNER must be exactly `1`, found {value:?}"),
        Err(env::VarError::NotUnicode(_)) => {
            panic!("CAST_DELEGATED_FIXTURE_RUNNER must be the UTF-8 value `1`")
        }
    }
}

/// Publish the deterministic CI receipt only after Mason returns from every
/// live build, decoded-bundle assertion, locked replan, rebuild, and reuse
/// comparison. Optional and single-fixture runs receive no proof path.
fn write_required_matrix_proof_if_requested() {
    let Some(proof_path) = env::var_os("CAST_FIXTURE_PROOF_PATH").map(PathBuf::from) else {
        return;
    };
    assert_eq!(env::var("CAST_REQUIRE_EXECUTION").as_deref(), Ok("1"));
    assert_eq!(env::var("CAST_EXECUTION_FIXTURE").as_deref(), Ok("all"));
    assert!(proof_path.is_absolute(), "fixture proof path must be absolute");
    assert_eq!(
        proof_path.file_name().and_then(|name| name.to_str()),
        Some(PROOF_FILE_NAME)
    );

    let commit = env::var("CAST_FIXTURE_GIT_COMMIT").expect("required fixture proof needs the exact Git commit");
    assert!(
        matches!(commit.len(), 40 | 64)
            && commit
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "fixture proof commit must be one canonical lowercase Git object ID"
    );

    let parent = proof_path.parent().expect("absolute fixture proof path has a parent");
    require_private_evidence_directory(parent);
    match fs::symlink_metadata(&proof_path) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => panic!("inspect absent fixture proof path {proof_path:?}: {source}"),
        Ok(_) => panic!("fixture proof path already exists: {proof_path:?}"),
    }

    let temporary = parent.join(PROOF_TEMP_FILE_NAME);
    let proof = format!(
        concat!(
            "{{\n",
            "  \"schema\": \"cast.fixtures-ci-proof.v1\",\n",
            "  \"git_commit\": \"{}\",\n",
            "  \"git_tree\": \"clean\",\n",
            "  \"selection\": \"all\",\n",
            "  \"required_execution\": true,\n",
            "  \"fixture_count\": 14,\n",
            "  \"fixtures\": [\n",
            "    \"autotools\",\n",
            "    \"autotools-options\",\n",
            "    \"cargo\",\n",
            "    \"cargo-features\",\n",
            "    \"cargo-vendored\",\n",
            "    \"cmake\",\n",
            "    \"custom\",\n",
            "    \"daemon-generated\",\n",
            "    \"factory-override\",\n",
            "    \"generated-config\",\n",
            "    \"hooks-patch\",\n",
            "    \"meson\",\n",
            "    \"split\",\n",
            "    \"userspace-profile\"\n",
            "  ],\n",
            "  \"assertions\": [\n",
            "    \"contentful-build-and-publish\",\n",
            "    \"decoded-bundle-contract\",\n",
            "    \"locked-plan-and-derivation-reuse\",\n",
            "    \"second-contentful-build-reused\",\n",
            "    \"stone-and-manifest-bytes-identical\"\n",
            "  ],\n",
            "  \"result\": \"passed\"\n",
            "}}\n"
        ),
        commit
    );

    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .unwrap_or_else(|source| panic!("create exclusive fixture proof temporary {temporary:?}: {source}"));
    output
        .write_all(proof.as_bytes())
        .unwrap_or_else(|source| panic!("write fixture proof temporary {temporary:?}: {source}"));
    output
        .sync_all()
        .unwrap_or_else(|source| panic!("sync fixture proof temporary {temporary:?}: {source}"));
    output
        .set_permissions(fs::Permissions::from_mode(0o644))
        .unwrap_or_else(|source| panic!("normalize fixture proof mode {temporary:?}: {source}"));
    output
        .sync_all()
        .unwrap_or_else(|source| panic!("sync normalized fixture proof {temporary:?}: {source}"));
    drop(output);
    fs::rename(&temporary, &proof_path)
        .unwrap_or_else(|source| panic!("publish fixture proof {proof_path:?}: {source}"));
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .unwrap_or_else(|source| panic!("sync fixture proof directory {parent:?}: {source}"));
}

fn require_private_evidence_directory(path: &Path) {
    let metadata = fs::symlink_metadata(path)
        .unwrap_or_else(|source| panic!("inspect fixture evidence directory {path:?}: {source}"));
    assert!(
        metadata.file_type().is_dir(),
        "fixture evidence path is not a directory: {path:?}"
    );
    assert_eq!(
        metadata.permissions().mode() & 0o7777,
        0o700,
        "fixture evidence directory must have mode 0700"
    );
}
