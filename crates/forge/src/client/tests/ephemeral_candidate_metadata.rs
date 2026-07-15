//! Adversarial proofs for production-path ephemeral candidate metadata.

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use super::*;

#[test]
fn ephemeral_candidate_metadata_never_follows_lib_or_os_info_symlinks() {
    for escape in ["lib", "os-info.json"] {
        let fixture = ephemeral_metadata_fixture();
        let candidate = fixture.materialize_candidate();
        let external = fixture.temporary.path().join(format!("external-{escape}-target"));
        let candidate_lib = fixture.root.join("usr/lib");

        if escape == "lib" {
            fs::create_dir(&external).unwrap();
            fs::write(external.join("sentinel"), b"external-directory").unwrap();
            symlink(&external, &candidate_lib).unwrap();
        } else {
            fs::write(&external, b"external-input").unwrap();
            fs::create_dir(&candidate_lib).unwrap();
            symlink(&external, candidate_lib.join("os-info.json")).unwrap();
        }

        assert_ephemeral_metadata_error(
            fixture
                .client
                .apply_ephemeral_candidate(candidate, generated_system_snapshot("ephemeral-metadata-package"))
                .unwrap_err(),
        );

        if escape == "lib" {
            assert_eq!(fs::read(external.join("sentinel")).unwrap(), b"external-directory");
            assert!(!external.join("os-release").exists());
            assert!(!external.join("system-model.glu").exists());
            assert!(fs::symlink_metadata(&candidate_lib).unwrap().file_type().is_symlink());
        } else {
            assert_eq!(fs::read(&external).unwrap(), b"external-input");
            assert!(
                fs::symlink_metadata(candidate_lib.join("os-info.json"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }
}

#[test]
fn ephemeral_candidate_metadata_never_follows_output_symlinks() {
    for output in ["os-release", "system-model.glu"] {
        let fixture = ephemeral_metadata_fixture();
        let candidate = fixture.materialize_candidate();
        let external = fixture.temporary.path().join(format!("external-{output}-target"));
        fs::write(&external, format!("external-{output}")).unwrap();
        let external_identity = inode_identity(&external);
        let lib = fixture.root.join("usr/lib");
        fs::create_dir(&lib).unwrap();
        symlink(&external, lib.join(output)).unwrap();

        assert_ephemeral_metadata_error(
            fixture
                .client
                .apply_ephemeral_candidate(candidate, generated_system_snapshot("ephemeral-metadata-package"))
                .unwrap_err(),
        );

        assert_eq!(inode_identity(&external), external_identity);
        assert_eq!(fs::read(&external).unwrap(), format!("external-{output}").as_bytes());
        assert!(fs::symlink_metadata(lib.join(output)).unwrap().file_type().is_symlink());
    }
}

#[test]
fn ephemeral_candidate_metadata_preserves_existing_output_inodes() {
    for output in ["os-release", "system-model.glu"] {
        for hardlinked in [false, true] {
            let fixture = ephemeral_metadata_fixture();
            let candidate = fixture.materialize_candidate();
            let lib = fixture.root.join("usr/lib");
            let candidate_output = lib.join(output);
            fs::create_dir(&lib).unwrap();

            let external = fixture.temporary.path().join(format!("external-{output}-{hardlinked}"));
            if hardlinked {
                fs::write(&external, format!("external-{output}")).unwrap();
                fs::hard_link(&external, &candidate_output).unwrap();
            } else {
                fs::write(&candidate_output, format!("candidate-occupant-{output}")).unwrap();
            }
            let occupant_identity = inode_identity(&candidate_output);
            let occupant_bytes = fs::read(&candidate_output).unwrap();

            assert_ephemeral_metadata_error(
                fixture
                    .client
                    .apply_ephemeral_candidate(candidate, generated_system_snapshot("ephemeral-metadata-package"))
                    .unwrap_err(),
            );

            assert_eq!(inode_identity(&candidate_output), occupant_identity);
            assert_eq!(fs::read(&candidate_output).unwrap(), occupant_bytes);
            if hardlinked {
                assert_eq!(inode_identity(&external), occupant_identity);
                assert_eq!(fs::read(&external).unwrap(), occupant_bytes);
            }
        }
    }
}

#[test]
fn ephemeral_candidate_metadata_final_name_races_are_no_replace() {
    for output in ["os-release", "system-model.glu"] {
        let fixture = ephemeral_metadata_fixture();
        let candidate = fixture.materialize_candidate();
        let external = fixture.temporary.path().join(format!("external-{output}-race"));
        fs::write(&external, format!("racing-{output}")).unwrap();
        let external_identity = inode_identity(&external);
        let hook_external = external.clone();
        let hook_output = fixture.root.join("usr/lib").join(output);
        let hook_ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let hook_observation = std::rc::Rc::clone(&hook_ran);
        candidate_metadata::arm_before_publication(output, move || {
            hook_observation.set(true);
            fs::hard_link(&hook_external, &hook_output).unwrap();
        });

        let error = fixture
            .client
            .apply_ephemeral_candidate(candidate, generated_system_snapshot("ephemeral-metadata-package"))
            .unwrap_err();

        assert!(
            hook_ran.get(),
            "publication race hook did not run for {output}: {error:#?}"
        );
        assert_ephemeral_metadata_error(error);
        assert_eq!(inode_identity(&external), external_identity);
        assert_eq!(fs::read(&external).unwrap(), format!("racing-{output}").as_bytes());
        assert_eq!(
            inode_identity(&fixture.root.join("usr/lib").join(output)),
            external_identity
        );
    }
}

#[test]
fn ephemeral_candidate_metadata_rejects_first_output_deletion_or_replacement() {
    for mutation in ["delete", "replace"] {
        let fixture = ephemeral_metadata_fixture();
        let candidate = fixture.materialize_candidate();
        let release = fixture.root.join("usr/lib/os-release");
        let external = fixture
            .temporary
            .path()
            .join(format!("external-first-output-{mutation}"));
        if mutation == "replace" {
            fs::write(&external, b"external-first-output-replacement").unwrap();
        }
        let hook_release = release.clone();
        let hook_external = external.clone();
        let hook_ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let hook_observation = std::rc::Rc::clone(&hook_ran);
        candidate_metadata::arm_after_first_publication(move || {
            hook_observation.set(true);
            fs::remove_file(&hook_release).unwrap();
            if mutation == "replace" {
                fs::hard_link(&hook_external, &hook_release).unwrap();
            }
        });

        let error = fixture
            .client
            .apply_ephemeral_candidate(candidate, generated_system_snapshot("ephemeral-metadata-package"))
            .unwrap_err();

        assert!(
            hook_ran.get(),
            "first-output mutation hook did not run for {mutation}: {error:#?}"
        );
        assert_ephemeral_metadata_error(error);

        match mutation {
            "delete" => assert!(!release.exists()),
            "replace" => {
                assert_eq!(fs::read(&external).unwrap(), b"external-first-output-replacement");
                assert_eq!(inode_identity(&release), inode_identity(&external));
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn retained_ephemeral_metadata_proof_rejects_post_transaction_mutation() {
    for mutation in ["rewrite", "delete", "replace", "hardlink"] {
        let fixture = ephemeral_metadata_fixture();
        let candidate = fixture.materialize_candidate();
        let output = fixture.root.join("usr/lib/system-model.glu");
        let external = fixture
            .temporary
            .path()
            .join(format!("external-post-transaction-{mutation}"));
        let hook_ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let hook_observation = std::rc::Rc::clone(&hook_ran);
        let hook_output = output.clone();
        let hook_external = external.clone();
        arm_after_ephemeral_transaction_triggers(move || {
            hook_observation.set(true);
            match mutation {
                "rewrite" => fs::write(&hook_output, b"rewritten-after-transaction").unwrap(),
                "delete" => fs::remove_file(&hook_output).unwrap(),
                "replace" => {
                    fs::write(&hook_external, b"replacement-after-transaction").unwrap();
                    fs::remove_file(&hook_output).unwrap();
                    fs::hard_link(&hook_external, &hook_output).unwrap();
                }
                "hardlink" => fs::hard_link(&hook_output, &hook_external).unwrap(),
                _ => unreachable!(),
            }
        });

        let error = fixture
            .client
            .apply_ephemeral_candidate(candidate, generated_system_snapshot("ephemeral-metadata-package"))
            .unwrap_err();

        assert!(hook_ran.get(), "transaction boundary hook did not run for {mutation}");
        assert_ephemeral_metadata_error(error);
        assert_eq!(
            take_observed_trigger_scopes(),
            ["transaction"],
            "system phase ran after {mutation} corrupted retained metadata"
        );
        match mutation {
            "rewrite" => assert_eq!(fs::read(&output).unwrap(), b"rewritten-after-transaction"),
            "delete" => assert!(!output.exists()),
            "replace" => {
                assert_eq!(fs::read(&external).unwrap(), b"replacement-after-transaction");
                assert_eq!(inode_identity(&output), inode_identity(&external));
            }
            "hardlink" => {
                assert_eq!(inode_identity(&output), inode_identity(&external));
                assert_eq!(fs::metadata(&output).unwrap().nlink(), 2);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn retained_ephemeral_metadata_proof_rejects_post_system_mutation() {
    let fixture = ephemeral_metadata_fixture();
    let candidate = fixture.materialize_candidate();
    let output = fixture.root.join("usr/lib/system-model.glu");
    let hook_ran = std::rc::Rc::new(std::cell::Cell::new(false));
    let hook_observation = std::rc::Rc::clone(&hook_ran);
    let hook_output = output.clone();
    arm_after_ephemeral_system_triggers(move || {
        hook_observation.set(true);
        fs::write(&hook_output, b"rewritten-after-system").unwrap();
    });

    let error = fixture
        .client
        .apply_ephemeral_candidate(candidate, generated_system_snapshot("ephemeral-metadata-package"))
        .unwrap_err();

    assert!(hook_ran.get(), "system boundary hook did not run");
    assert_ephemeral_metadata_error(error);
    assert_eq!(take_observed_trigger_scopes(), ["transaction", "system"]);
    assert_eq!(fs::read(&output).unwrap(), b"rewritten-after-system");
}

#[test]
fn target_or_usr_substitution_before_transaction_discovery_cannot_reach_replacement_triggers() {
    for substitution in [EphemeralSubstitution::Target, EphemeralSubstitution::Usr] {
        for replacement in [ReplacementTrigger::Invalid, ReplacementTrigger::Destructive] {
            let fixture = ephemeral_metadata_fixture();
            let candidate = fixture.materialize_trigger_candidate();
            let snapshot = generated_system_snapshot("ephemeral-metadata-package");
            let expected_snapshot = snapshot.encoded().to_owned();
            let original_usr = fixture.root.join("usr");
            let retained_marker = original_usr.join("retained-transaction-marker");
            fs::write(&retained_marker, b"retained-transaction-marker").unwrap();
            write_destructive_trigger(
                &original_usr,
                "tx",
                "retained-transaction",
                "/usr/retained-transaction-marker",
            );

            let selected_identity = inode_identity(&substitution.selected_path(&fixture.root));
            let detached = fixture.temporary.path().join(format!(
                "detached-before-transaction-{}-{}",
                substitution.name(),
                replacement.name()
            ));
            let hook_ran = std::rc::Rc::new(std::cell::Cell::new(false));
            let hook_observation = std::rc::Rc::clone(&hook_ran);
            let hook_root = fixture.root.clone();
            let hook_detached = detached.clone();
            arm_before_ephemeral_transaction_triggers(move || {
                hook_observation.set(true);
                substitution.replace_selected(&hook_root, &hook_detached);
                prepare_replacement_usr(&hook_root, "tx", replacement, "replacement-transaction-marker");
            });

            let error = fixture
                .client
                .apply_ephemeral_candidate(candidate, snapshot)
                .unwrap_err();

            assert!(
                hook_ran.get(),
                "pre-transaction substitution hook did not run for {} {}",
                substitution.name(),
                replacement.name()
            );
            assert_ephemeral_trigger_boundary_error(error, &fixture.root);
            assert_eq!(take_observed_trigger_scopes(), ["transaction"]);
            assert_substitution_preserved_both_sides(
                &fixture.root,
                &detached,
                substitution,
                selected_identity,
                "retained-transaction-marker",
                "replacement-transaction-marker",
                &expected_snapshot,
            );
        }
    }
}

#[test]
fn target_or_usr_substitution_between_phases_cannot_reach_replacement_system_triggers() {
    for substitution in [EphemeralSubstitution::Target, EphemeralSubstitution::Usr] {
        for replacement in [ReplacementTrigger::Invalid, ReplacementTrigger::Destructive] {
            let fixture = ephemeral_metadata_fixture();
            let candidate = fixture.materialize_trigger_candidate();
            let snapshot = generated_system_snapshot("ephemeral-metadata-package");
            let expected_snapshot = snapshot.encoded().to_owned();
            let original_usr = fixture.root.join("usr");
            let retained_marker = original_usr.join("retained-system-marker");
            fs::write(&retained_marker, b"retained-system-marker").unwrap();
            write_destructive_trigger(&original_usr, "sys", "retained-system", "/usr/retained-system-marker");

            let selected_identity = inode_identity(&substitution.selected_path(&fixture.root));
            let detached = fixture.temporary.path().join(format!(
                "detached-before-system-{}-{}",
                substitution.name(),
                replacement.name()
            ));
            let hook_ran = std::rc::Rc::new(std::cell::Cell::new(false));
            let hook_observation = std::rc::Rc::clone(&hook_ran);
            let hook_root = fixture.root.clone();
            let hook_detached = detached.clone();
            arm_before_ephemeral_system_triggers(move || {
                hook_observation.set(true);
                substitution.replace_selected(&hook_root, &hook_detached);
                prepare_replacement_usr(&hook_root, "sys", replacement, "replacement-system-marker");
            });

            let error = fixture
                .client
                .apply_ephemeral_candidate(candidate, snapshot)
                .unwrap_err();

            assert!(
                hook_ran.get(),
                "pre-system substitution hook did not run for {} {}",
                substitution.name(),
                replacement.name()
            );
            assert_ephemeral_trigger_boundary_error(error, &fixture.root);
            assert_eq!(take_observed_trigger_scopes(), ["transaction", "system"]);
            assert_substitution_preserved_both_sides(
                &fixture.root,
                &detached,
                substitution,
                selected_identity,
                "retained-system-marker",
                "replacement-system-marker",
                &expected_snapshot,
            );
        }
    }
}

#[test]
fn successful_ephemeral_metadata_is_exact_evaluable_and_root_abi_complete() {
    let fixture = ephemeral_metadata_fixture();
    let candidate = fixture.materialize_candidate();
    let snapshot = generated_system_snapshot("ephemeral-metadata-package");
    let expected_snapshot = snapshot.encoded().to_owned();

    fixture.client.apply_ephemeral_candidate(candidate, snapshot).unwrap();

    assert_eq!(
        fs::read_to_string(fixture.root.join("usr/lib/os-release")).unwrap(),
        candidate_metadata::GENERIC_OS_RELEASE
    );
    assert_generated_snapshot(
        &fixture.root.join("usr/lib/system-model.glu"),
        &expected_snapshot,
        "ephemeral-metadata-package",
    );
    for output in ["os-release", "system-model.glu"] {
        let metadata = fs::symlink_metadata(fixture.root.join("usr/lib").join(output)).unwrap();
        assert!(metadata.file_type().is_file(), "metadata {output}");
        assert_eq!(metadata.uid(), unsafe { nix::libc::geteuid() }, "metadata {output}");
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o644, "metadata {output}");
        assert_eq!(metadata.nlink(), 1, "metadata {output}");
    }
    assert_root_abi_links(&fixture.root);
    assert_root_abi_links(&fixture.client.installation.isolation_dir());
}

#[derive(Clone, Copy)]
enum EphemeralSubstitution {
    Target,
    Usr,
}

impl EphemeralSubstitution {
    fn name(self) -> &'static str {
        match self {
            Self::Target => "target",
            Self::Usr => "usr",
        }
    }

    fn selected_path(self, root: &Path) -> PathBuf {
        match self {
            Self::Target => root.to_owned(),
            Self::Usr => root.join("usr"),
        }
    }

    fn replace_selected(self, root: &Path, detached: &Path) {
        match self {
            Self::Target => fs::rename(root, detached).unwrap(),
            Self::Usr => fs::rename(root.join("usr"), detached).unwrap(),
        }
    }

    fn retained_usr(self, _root: &Path, detached: &Path) -> PathBuf {
        match self {
            Self::Target => detached.join("usr"),
            Self::Usr => detached.to_owned(),
        }
    }

    fn retained_selected(self, root: &Path, detached: &Path) -> PathBuf {
        match self {
            Self::Target => detached.to_owned(),
            Self::Usr => self.retained_usr(root, detached),
        }
    }
}

#[derive(Clone, Copy)]
enum ReplacementTrigger {
    Invalid,
    Destructive,
}

impl ReplacementTrigger {
    fn name(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Destructive => "destructive",
        }
    }
}

struct EphemeralMetadataFixture {
    temporary: tempfile::TempDir,
    client: Client,
    root: PathBuf,
}

impl EphemeralMetadataFixture {
    fn materialize_candidate(&self) -> EphemeralCandidate {
        self.client
            .materialize_ephemeral_candidate(std::iter::empty::<&package::Id>())
            .unwrap()
    }

    fn materialize_trigger_candidate(&self) -> EphemeralCandidate {
        let package = package::Id::from("ephemeral-trigger-boundary");
        self.client
            .layout_db
            .add(
                &package,
                &StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFDIR | 0o755,
                    tag: 0,
                    file: StonePayloadLayoutFile::Directory("share/ephemeral-trigger-input".into()),
                },
            )
            .unwrap();
        self.client.materialize_ephemeral_candidate([&package]).unwrap()
    }
}

fn ephemeral_metadata_fixture() -> EphemeralMetadataFixture {
    let temporary = tempfile::tempdir().unwrap();
    prepare_private_installation_root(temporary.path());
    let installation_root = temporary.path().join("installation");
    let root = temporary.path().join("ephemeral");
    fs::create_dir(&installation_root).unwrap();
    let installation = test_installation(&installation_root);
    let client = Client::builder("ephemeral-candidate-metadata-test", installation)
        .repositories(repository::Map::default())
        .ephemeral(&root)
        .build()
        .unwrap();

    EphemeralMetadataFixture {
        temporary,
        client,
        root,
    }
}

fn assert_ephemeral_metadata_error(error: Error) {
    assert!(
        matches!(error, Error::EphemeralCandidateMetadata { .. }),
        "unexpected ephemeral candidate metadata error: {error:#?}"
    );
}

fn assert_ephemeral_trigger_boundary_error(error: Error, root: &Path) {
    assert!(
        matches!(
            &error,
            Error::PostBlit(postblit::Error::PinRetainedEphemeralSource {
                role: "external candidate trigger view",
                path,
                ..
            }) if path == root
        ),
        "unexpected retained ephemeral trigger boundary error: {error:#?}"
    );
}

fn prepare_replacement_usr(root: &Path, domain: &str, replacement: ReplacementTrigger, marker: &str) {
    let usr = root.join("usr");
    fs::create_dir_all(usr.join("share/ephemeral-trigger-input")).unwrap();
    fs::write(root.join("replacement-sentinel"), b"replacement-root").unwrap();
    fs::write(usr.join(marker), marker.as_bytes()).unwrap();
    match replacement {
        ReplacementTrigger::Invalid => {
            let trigger = usr.join(format!("share/cast/triggers/{domain}.d/replacement-invalid.glu"));
            fs::create_dir_all(trigger.parent().unwrap()).unwrap();
            fs::write(trigger, b"let replacement_trigger =").unwrap();
        }
        ReplacementTrigger::Destructive => {
            write_destructive_trigger(&usr, domain, "replacement-destructive", &format!("/usr/{marker}"))
        }
    }
}

fn write_destructive_trigger(usr: &Path, domain: &str, name: &str, marker: &str) {
    let trigger = usr.join(format!("share/cast/triggers/{domain}.d/{name}.glu"));
    fs::create_dir_all(trigger.parent().unwrap()).unwrap();
    fs::write(
        trigger,
        format!(
            r#"let cast = import! cast.trigger.v1
let base = cast.trigger "{name}" "Ephemeral trigger authority boundary proof"
{{
    paths = [cast.path
        "/usr/share/ephemeral-trigger-input"
        ["delete-marker"]
        (cast.optional.set cast.path_kind.directory)],
    handlers = [cast.handler.named "delete-marker" (cast.handler.delete
        ["{marker}"])],
    .. base
}}
"#
        ),
    )
    .unwrap();
}

fn assert_substitution_preserved_both_sides(
    root: &Path,
    detached: &Path,
    substitution: EphemeralSubstitution,
    selected_identity: (u64, u64),
    retained_marker: &str,
    replacement_marker: &str,
    expected_snapshot: &str,
) {
    let retained_selected = substitution.retained_selected(root, detached);
    let retained_usr = substitution.retained_usr(root, detached);
    let replacement_selected = substitution.selected_path(root);
    assert_eq!(inode_identity(&retained_selected), selected_identity);
    assert_ne!(inode_identity(&replacement_selected), selected_identity);
    assert_eq!(
        fs::read(retained_usr.join(retained_marker)).unwrap(),
        retained_marker.as_bytes()
    );
    assert_eq!(
        fs::read_to_string(retained_usr.join("lib/os-release")).unwrap(),
        candidate_metadata::GENERIC_OS_RELEASE
    );
    assert_eq!(
        fs::read_to_string(retained_usr.join("lib/system-model.glu")).unwrap(),
        expected_snapshot
    );
    assert_eq!(
        fs::read(root.join("replacement-sentinel")).unwrap(),
        b"replacement-root"
    );
    assert_eq!(
        fs::read(root.join("usr").join(replacement_marker)).unwrap(),
        replacement_marker.as_bytes()
    );
    assert!(!root.join("usr/lib/os-release").exists());
    assert!(!root.join("usr/lib/system-model.glu").exists());
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}
