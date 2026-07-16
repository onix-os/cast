use std::{
    fs::{self, File, Permissions},
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use super::*;

const RELEASE: &[u8] = b"NAME=\"Neutral metadata test\"\n";
const SNAPSHOT: &[u8] = b"{ packages = [] }\n";

#[derive(Debug, Eq, PartialEq)]
struct EntryEvidence {
    relative: PathBuf,
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    links: u64,
    length: u64,
    bytes: Vec<u8>,
}

#[test]
fn same_candidate_proof_accepts_exact_inode_and_rejects_same_layout_foreign_candidate_without_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let exact_path = temporary.path().join("exact-usr");
    let foreign_path = temporary.path().join("foreign-usr");
    create_candidate_root(&exact_path);
    create_candidate_root(&foreign_path);

    let exact = File::open(&exact_path).unwrap();
    let proof = CandidateMetadataPublication::begin(&exact, &exact_path, SNAPSHOT)
        .unwrap()
        .publish(RELEASE)
        .unwrap();
    mirror_logical_metadata(&foreign_path);
    assert_eq!(logical_layout(&exact_path), logical_layout(&foreign_path));

    let reopened_exact = File::open(&exact_path).unwrap();
    proof.require_same_candidate(&reopened_exact, &exact_path).unwrap();

    let exact_before = retained_evidence(&exact_path);
    let foreign_before = retained_evidence(&foreign_path);
    let foreign = File::open(&foreign_path).unwrap();
    let error = proof.require_same_candidate(&foreign, &foreign_path).unwrap_err();

    assert!(
        matches!(error, CandidateMetadataError::DirectoryChanged { ref path } if path == &foreign_path),
        "foreign candidate returned the wrong error: {error:#?}"
    );
    assert_eq!(retained_evidence(&exact_path), exact_before);
    assert_eq!(retained_evidence(&foreign_path), foreign_before);
}

fn create_candidate_root(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
}

fn mirror_logical_metadata(candidate: &Path) {
    let lib = candidate.join("lib");
    fs::create_dir(&lib).unwrap();
    fs::set_permissions(&lib, Permissions::from_mode(0o755)).unwrap();
    for (name, bytes) in [("os-release", RELEASE), ("system-model.glu", SNAPSHOT)] {
        let path = lib.join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(path, Permissions::from_mode(0o644)).unwrap();
    }
}

fn logical_layout(candidate: &Path) -> Vec<(PathBuf, u32, u32, u64, u64, Vec<u8>)> {
    retained_evidence(candidate)
        .into_iter()
        .map(|entry| {
            (
                entry.relative,
                entry.owner,
                entry.mode,
                entry.links,
                entry.length,
                entry.bytes,
            )
        })
        .collect()
}

fn retained_evidence(candidate: &Path) -> Vec<EntryEvidence> {
    ["", "lib", "lib/os-release", "lib/system-model.glu"]
        .into_iter()
        .map(|relative| {
            let path = candidate.join(relative);
            let metadata = fs::symlink_metadata(&path).unwrap();
            EntryEvidence {
                relative: PathBuf::from(relative),
                device: metadata.dev(),
                inode: metadata.ino(),
                owner: metadata.uid(),
                mode: metadata.permissions().mode() & 0o7777,
                links: metadata.nlink(),
                length: metadata.len(),
                bytes: if metadata.file_type().is_file() {
                    fs::read(path).unwrap()
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}
