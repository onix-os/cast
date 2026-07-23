//! Bounded atomic publication of the required matrix proof.

use std::{
    ffi::CString,
    fs::Permissions,
    io,
    io::Write as _,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::Path,
};

#[cfg(feature = "delegated-fixture-test-support")]
use std::{env, path::PathBuf};

use fs_err as fs;
use fs_err::os::unix::fs::OpenOptionsExt as _;
use serde::Serialize;

use super::{FixtureEvidence, MatrixTotals, ledger::BUNDLE_LEDGER_SCHEMA, require_sha256};

const PROOF_SCHEMA: &str = "cast.fixtures-ci-proof.v2";
const PROOF_FILE_NAME: &str = "fixtures-ci-proof.json";
const PROOF_TEMP_FILE_NAME: &str = ".fixtures-ci-proof.json.tmp";
pub(super) const MAX_PROOF_BYTES: usize = 131_072;

#[derive(Serialize)]
struct FixtureCiProof<'a> {
    schema: &'static str,
    git_commit: &'a str,
    git_tree: &'static str,
    selection: &'static str,
    required_execution: bool,
    bundle_ledger_schema: &'static str,
    totals: &'a MatrixTotals,
    fixtures: &'a [FixtureEvidence],
    result: &'static str,
}

#[cfg(feature = "delegated-fixture-test-support")]
pub(super) fn publish(fixtures: &[FixtureEvidence], totals: &MatrixTotals) {
    let proof_path = env::var_os("CAST_FIXTURE_PROOF_PATH")
        .map(PathBuf::from)
        .expect("required all-fixture execution needs CAST_FIXTURE_PROOF_PATH");
    let commit =
        env::var("CAST_FIXTURE_GIT_COMMIT").expect("required all-fixture execution needs CAST_FIXTURE_GIT_COMMIT");
    publish_to(&proof_path, &commit, fixtures, totals);
}

fn publish_to(path: &Path, commit: &str, fixtures: &[FixtureEvidence], totals: &MatrixTotals) {
    assert!(path.is_absolute(), "fixture proof path must be absolute");
    assert_eq!(path.file_name().and_then(|name| name.to_str()), Some(PROOF_FILE_NAME));
    require_sha256_or_git_oid(commit);
    let parent = path.parent().expect("absolute fixture proof path has a parent");
    require_private_evidence_directory(parent);
    require_absent(path, "fixture proof path");
    let temporary = parent.join(PROOF_TEMP_FILE_NAME);
    require_absent(&temporary, "fixture proof temporary path");

    let mut bytes = serde_json::to_vec_pretty(&FixtureCiProof {
        schema: PROOF_SCHEMA,
        git_commit: commit,
        git_tree: "clean",
        selection: "all",
        required_execution: true,
        bundle_ledger_schema: BUNDLE_LEDGER_SCHEMA,
        totals,
        fixtures,
        result: "passed",
    })
    .expect("serialize fixture CI proof");
    bytes.push(b'\n');
    assert!(
        !bytes.is_empty() && bytes.len() <= MAX_PROOF_BYTES,
        "fixture proof exceeds its byte boundary"
    );

    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .unwrap_or_else(|source| panic!("create exclusive fixture proof temporary {temporary:?}: {source}"));
    output
        .write_all(&bytes)
        .unwrap_or_else(|source| panic!("write fixture proof temporary {temporary:?}: {source}"));
    output
        .sync_all()
        .unwrap_or_else(|source| panic!("sync fixture proof temporary {temporary:?}: {source}"));
    output
        .set_permissions(Permissions::from_mode(0o644))
        .unwrap_or_else(|source| panic!("normalize fixture proof mode {temporary:?}: {source}"));
    output
        .sync_all()
        .unwrap_or_else(|source| panic!("sync normalized fixture proof {temporary:?}: {source}"));
    drop(output);
    rename_noreplace(&temporary, path)
        .unwrap_or_else(|source| panic!("publish fixture proof without replacement {path:?}: {source}"));
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .unwrap_or_else(|source| panic!("sync fixture proof directory {parent:?}: {source}"));
}

fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "fixture proof temporary path contains NUL"))?;
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "fixture proof path contains NUL"))?;
    // SAFETY: both C strings remain live for this one syscall. RENAME_NOREPLACE
    // either publishes the staged inode at an absent name or changes nothing.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            nix::libc::AT_FDCWD,
            source.as_ptr(),
            nix::libc::AT_FDCWD,
            target.as_ptr(),
            nix::libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn require_sha256_or_git_oid(value: &str) {
    assert!(
        matches!(value.len(), 40 | 64),
        "fixture proof commit has an unsupported object-ID length"
    );
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "fixture proof commit must be canonical lowercase hexadecimal"
    );
    if value.len() == 64 {
        require_sha256(value, "fixture proof commit");
    }
}

fn require_private_evidence_directory(path: &Path) {
    let metadata = fs::symlink_metadata(path)
        .unwrap_or_else(|source| panic!("inspect fixture evidence directory {path:?}: {source}"));
    assert!(
        metadata.file_type().is_dir(),
        "fixture evidence path is not a directory: {path:?}"
    );
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    assert_eq!(
        metadata.uid(),
        unsafe { nix::libc::geteuid() },
        "fixture evidence directory must be owned by the proof producer"
    );
    assert_eq!(
        metadata.permissions().mode() & 0o7777,
        0o700,
        "fixture evidence directory must have mode 0700"
    );
}

fn require_absent(path: &Path, role: &str) {
    match fs::symlink_metadata(path) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => panic!("inspect absent {role} {path:?}: {source}"),
        Ok(_) => panic!("{role} already exists: {path:?}"),
    }
}

#[cfg(test)]
pub(super) fn serialize_for_test(fixtures: &[FixtureEvidence], totals: &MatrixTotals, commit: &str) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(&FixtureCiProof {
        schema: PROOF_SCHEMA,
        git_commit: commit,
        git_tree: "clean",
        selection: "all",
        required_execution: true,
        bundle_ledger_schema: BUNDLE_LEDGER_SCHEMA,
        totals,
        fixtures,
        result: "passed",
    })
    .unwrap();
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
pub(super) fn publish_to_for_test(path: &Path, commit: &str, fixtures: &[FixtureEvidence], totals: &MatrixTotals) {
    publish_to(path, commit, fixtures, totals);
}

#[cfg(test)]
pub(super) fn rename_noreplace_for_test(source: &Path, target: &Path) -> io::Result<()> {
    rename_noreplace(source, target)
}
