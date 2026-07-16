use std::{
    fs::{self, File, Permissions},
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
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
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
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

#[test]
fn existing_metadata_verification_proves_independent_bytes_without_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let candidate_path = temporary.path().join("archived-usr");
    create_candidate_root(&candidate_path);
    mirror_logical_metadata(&candidate_path);
    let before = retained_evidence(&candidate_path);

    let candidate = File::open(&candidate_path).unwrap();
    let verification = CandidateMetadataVerification::begin(&candidate, &candidate_path, SNAPSHOT).unwrap();
    assert_eq!(verification.read_optional_os_info().unwrap(), None);
    let proof = verification.prove(RELEASE).unwrap();
    proof.require_same_candidate(&candidate, &candidate_path).unwrap();

    assert_eq!(retained_evidence(&candidate_path), before);
}

#[test]
fn existing_metadata_verification_rejects_wrong_independent_bytes_without_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let candidate_path = temporary.path().join("archived-usr");
    create_candidate_root(&candidate_path);
    mirror_logical_metadata(&candidate_path);
    let before = retained_evidence(&candidate_path);
    let mut wrong_release = RELEASE.to_vec();
    wrong_release[0] = b'X';

    let candidate = File::open(&candidate_path).unwrap();
    let verification = CandidateMetadataVerification::begin(&candidate, &candidate_path, SNAPSHOT).unwrap();
    let error = verification.prove(&wrong_release).unwrap_err();

    assert!(matches!(error, CandidateMetadataError::FileChanged { .. }));
    assert_eq!(retained_evidence(&candidate_path), before);
}

#[test]
fn existing_metadata_verification_rejects_same_byte_release_replacement_during_proof() {
    let temporary = tempfile::tempdir().unwrap();
    let candidate_path = temporary.path().join("archived-usr");
    let parked_release = temporary.path().join("parked-os-release");
    create_candidate_root(&candidate_path);
    mirror_logical_metadata(&candidate_path);
    let canonical_release = candidate_path.join("lib/os-release");
    let original = fs::symlink_metadata(&canonical_release).unwrap();

    let hook_release = canonical_release.clone();
    let hook_parked = parked_release.clone();
    arm_after_existing_release_retained(move || {
        fs::rename(&hook_release, &hook_parked).unwrap();
        fs::write(&hook_release, RELEASE).unwrap();
        fs::set_permissions(&hook_release, Permissions::from_mode(0o644)).unwrap();
    });

    let candidate = File::open(&candidate_path).unwrap();
    let verification = CandidateMetadataVerification::begin(&candidate, &candidate_path, SNAPSHOT).unwrap();
    let error = verification.prove(RELEASE).unwrap_err();

    assert!(matches!(error, CandidateMetadataError::FileChanged { .. }));
    let parked = fs::symlink_metadata(&parked_release).unwrap();
    let replacement = fs::symlink_metadata(&canonical_release).unwrap();
    assert_eq!((parked.dev(), parked.ino()), (original.dev(), original.ino()));
    assert_ne!((replacement.dev(), replacement.ino()), (original.dev(), original.ino()));
    assert_eq!(fs::read(parked_release).unwrap(), RELEASE);
    assert_eq!(fs::read(canonical_release).unwrap(), RELEASE);
    assert_eq!(fs::read(candidate_path.join("lib/system-model.glu")).unwrap(), SNAPSHOT);
}

#[test]
fn existing_metadata_verification_rejects_unsafe_canonical_outputs_without_repair() {
    for unsafe_shape in ["writable", "hardlinked", "symlink"] {
        let temporary = tempfile::tempdir().unwrap();
        let candidate_path = temporary.path().join("archived-usr");
        create_candidate_root(&candidate_path);
        mirror_logical_metadata(&candidate_path);
        let canonical_release = candidate_path.join("lib/os-release");
        let external = temporary.path().join("external-release");
        match unsafe_shape {
            "writable" => fs::set_permissions(&canonical_release, Permissions::from_mode(0o666)).unwrap(),
            "hardlinked" => {
                fs::hard_link(&canonical_release, &external).unwrap();
            }
            "symlink" => {
                fs::write(&external, RELEASE).unwrap();
                fs::remove_file(&canonical_release).unwrap();
                symlink(&external, &canonical_release).unwrap();
            }
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&canonical_release).unwrap();

        let candidate = File::open(&candidate_path).unwrap();
        let verification = CandidateMetadataVerification::begin(&candidate, &candidate_path, SNAPSHOT).unwrap();
        let error = verification.prove(RELEASE).unwrap_err();

        assert!(matches!(error, CandidateMetadataError::FileChanged { .. }));
        let after = fs::symlink_metadata(&canonical_release).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(after.permissions().mode(), before.permissions().mode());
        assert_eq!(after.nlink(), before.nlink());
        if unsafe_shape == "symlink" {
            assert!(after.file_type().is_symlink());
            assert_eq!(fs::read(external).unwrap(), RELEASE);
        } else {
            assert_eq!(fs::read(canonical_release).unwrap(), RELEASE);
        }
    }
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
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
                bytes: if metadata.file_type().is_file() {
                    fs::read(path).unwrap()
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}
