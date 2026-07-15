//! Escape and aliasing proofs for descriptor-bound repair metadata.

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

use fs_err as fs;

use super::*;

#[test]
fn metadata_decoration_never_follows_a_candidate_lib_symlink() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let external = fixture.client.installation.root.join("usr/live-metadata-target");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("sentinel"), b"outside-candidate").unwrap();
    let candidate = fixture.empty_candidate();
    let staging = fixture.client.installation.staging_dir();
    record_state_id(&staging, fixture.repaired.id).unwrap();
    symlink(&external, staging.join("usr/lib")).unwrap();

    let preserved = expect_preserved_metadata_failure(&fixture, candidate, fixture.snapshot("lib-symlink-must-fail"));

    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(fs::read(external.join("sentinel")).unwrap(), b"outside-candidate");
    assert!(!external.join("os-release").exists());
    assert!(!external.join("system-model.glu").exists());
    assert!(
        fs::symlink_metadata(preserved.join("usr/lib"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_exact_empty_private_staging(&staging);
}

#[test]
fn metadata_decoration_never_follows_an_os_info_symlink() {
    let fixture = Fixture::new(false);
    let external = fixture.client.installation.root.join("usr/live-os-info.json");
    fs::write(&external, b"external-input-must-not-be-read-through-a-link").unwrap();
    let candidate = fixture.empty_candidate();
    let staging = fixture.client.installation.staging_dir();
    record_state_id(&staging, fixture.repaired.id).unwrap();
    let lib = staging.join("usr/lib");
    fs::create_dir_all(&lib).unwrap();
    symlink(&external, lib.join("os-info.json")).unwrap();

    let preserved =
        expect_preserved_metadata_failure(&fixture, candidate, fixture.snapshot("os-info-symlink-must-fail"));

    assert_eq!(
        fs::read(&external).unwrap(),
        b"external-input-must-not-be-read-through-a-link"
    );
    assert!(
        fs::symlink_metadata(preserved.join("usr/lib/os-info.json"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!fixture.archived_root.exists());
    assert_exact_empty_private_staging(&staging);
}

#[test]
fn existing_metadata_outputs_are_preserved_without_mutating_regular_or_hardlinked_inodes() {
    for output in ["os-release", "system-model.glu"] {
        let fixture = Fixture::new(true);
        let external = fixture
            .client
            .installation
            .root
            .join("usr")
            .join(format!("live-{output}"));
        fs::write(&external, format!("outside-{output}")).unwrap();
        let candidate = fixture.empty_candidate();
        let staging = fixture.client.installation.staging_dir();
        record_state_id(&staging, fixture.repaired.id).unwrap();
        let lib = staging.join("usr/lib");
        fs::create_dir_all(&lib).unwrap();
        fs::hard_link(&external, lib.join(output)).unwrap();
        let identity = inode_identity(&external);
        assert_eq!(fs::metadata(&external).unwrap().nlink(), 2);

        let preserved =
            expect_preserved_metadata_failure(&fixture, candidate, fixture.snapshot("metadata-hardlink-must-fail"));

        assert_eq!(inode_identity(&external), identity, "output {output}");
        assert_eq!(
            fs::read(&external).unwrap(),
            format!("outside-{output}").as_bytes(),
            "output {output}"
        );
        assert_eq!(
            inode_identity(&preserved.join("usr/lib").join(output)),
            identity,
            "output {output}"
        );
        assert_exact_empty_private_staging(&staging);
    }
}

#[test]
fn successful_metadata_publication_creates_independent_sealed_files() {
    let fixture = Fixture::new(false);

    fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("sealed-metadata"),
        )
        .unwrap();

    for name in ["os-release", "system-model.glu"] {
        let path = fixture.archived_root.join("usr/lib").join(name);
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(metadata.file_type().is_file(), "metadata {name}");
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o644, "metadata {name}");
        assert_eq!(metadata.nlink(), 1, "metadata {name}");
    }
}

#[test]
fn a_second_metadata_name_collision_preserves_the_partial_candidate_without_replacing_the_occupant() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let staging = fixture.client.installation.staging_dir();
    let external = fixture.client.installation.root.join("usr/live-system-model-collision");
    fs::write(&external, b"external-model-must-survive").unwrap();
    let external_identity = inode_identity(&external);
    let hook_external = external.clone();
    let hook_output = staging.join("usr/lib/system-model.glu");
    super::super::archived_repair_metadata::arm_after_first_publication(move || {
        fs::hard_link(&hook_external, &hook_output).unwrap();
    });

    let preserved = expect_preserved_metadata_failure(
        &fixture,
        fixture.empty_candidate(),
        fixture.snapshot("metadata-pair-collision"),
    );

    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(
        fs::read(fixture.archived_root.join("opaque-root-sentinel")).unwrap(),
        b"old-wrapper"
    );
    assert_eq!(inode_identity(&external), external_identity);
    assert_eq!(fs::read(&external).unwrap(), b"external-model-must-survive");
    assert!(preserved.join("usr/lib/os-release").is_file());
    assert_eq!(
        inode_identity(&preserved.join("usr/lib/system-model.glu")),
        external_identity
    );
    assert_eq!(
        fs::read(preserved.join("usr/lib/system-model.glu")).unwrap(),
        b"external-model-must-survive"
    );
    assert_eq!(
        fs::read(fixture.client.installation.root.join("usr/live-sentinel")).unwrap(),
        b"live"
    );
    assert_exact_empty_private_staging(&staging);
}

#[test]
fn deleting_the_first_metadata_output_during_pair_publication_preserves_the_partial_candidate() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let staging = fixture.client.installation.staging_dir();
    let hook_release = staging.join("usr/lib/os-release");
    super::super::archived_repair_metadata::arm_after_first_publication(move || {
        fs::remove_file(&hook_release).unwrap();
    });

    let preserved = expect_preserved_metadata_failure(
        &fixture,
        fixture.empty_candidate(),
        fixture.snapshot("metadata-first-output-deletion"),
    );

    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(
        fs::read(fixture.archived_root.join("opaque-root-sentinel")).unwrap(),
        b"old-wrapper"
    );
    assert!(!preserved.join("usr/lib/os-release").exists());
    assert!(preserved.join("usr/lib/system-model.glu").is_file());
    assert_eq!(
        fs::read(fixture.client.installation.root.join("usr/live-sentinel")).unwrap(),
        b"live"
    );
    assert_exact_empty_private_staging(&staging);
}

#[test]
fn replacing_the_first_metadata_output_during_pair_publication_never_adopts_the_occupant() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let staging = fixture.client.installation.staging_dir();
    let external = fixture.client.installation.root.join("usr/live-os-release-replacement");
    fs::write(&external, b"external-release-must-survive").unwrap();
    fs::set_permissions(&external, std::fs::Permissions::from_mode(0o644)).unwrap();
    let external_identity = inode_identity(&external);
    let hook_external = external.clone();
    let hook_release = staging.join("usr/lib/os-release");
    super::super::archived_repair_metadata::arm_after_first_publication(move || {
        fs::remove_file(&hook_release).unwrap();
        fs::hard_link(&hook_external, &hook_release).unwrap();
    });

    let preserved = expect_preserved_metadata_failure(
        &fixture,
        fixture.empty_candidate(),
        fixture.snapshot("metadata-first-output-replacement"),
    );

    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(inode_identity(&external), external_identity);
    assert_eq!(fs::read(&external).unwrap(), b"external-release-must-survive");
    assert_eq!(inode_identity(&preserved.join("usr/lib/os-release")), external_identity);
    assert_eq!(
        fs::read(preserved.join("usr/lib/os-release")).unwrap(),
        b"external-release-must-survive"
    );
    assert!(preserved.join("usr/lib/system-model.glu").is_file());
    assert_eq!(
        fs::read(fixture.client.installation.root.join("usr/live-sentinel")).unwrap(),
        b"live"
    );
    assert_exact_empty_private_staging(&staging);
}

fn expect_preserved_metadata_failure(
    fixture: &Fixture,
    candidate: super::super::archived_repair_materialization::ArchivedRepairCandidate,
    snapshot: SystemModel,
) -> PathBuf {
    let error = fixture
        .client
        .repair_archived_state(candidate, &fixture.repaired, snapshot)
        .unwrap_err();
    let RepairError::CandidatePreserved { quarantine, .. } = repair_error(error) else {
        panic!("metadata failure must preserve the exact candidate wrapper");
    };
    quarantine
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}
