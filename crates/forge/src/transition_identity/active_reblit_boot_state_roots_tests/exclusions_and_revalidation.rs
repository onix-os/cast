use std::{fs, os::unix::fs::PermissionsExt as _};

use super::*;

#[test]
fn absent_archive_remains_excluded_when_an_exact_wrapper_appears_later() {
    let fixture = Fixture::new();
    let archived = state(81);
    let prepared = fixture.prepare(&[head_state(), archived]).unwrap();

    assert_eq!(prepared.eligible_state_ids(), &[head_state()]);
    assert_eq!(prepared.exclusions().len(), 1);
    assert_eq!(prepared.exclusions()[0].state_id(), archived);
    assert_eq!(
        prepared.exclusions()[0].reason(),
        ArchivedBootStateRootExclusionReason::Absent
    );

    fixture.create_archive(archived);
    let revalidated = prepared.revalidate(&fixture.installation).unwrap();
    assert_eq!(prepared.eligible_state_ids(), &[head_state()]);
    assert_eq!(revalidated.roots().count(), 1);
}

#[test]
fn inexact_archive_remains_excluded_after_its_layout_is_repaired() {
    let fixture = Fixture::new();
    let archived = state(82);
    fixture.create_archive(archived);
    let residue = fixture.archive_path(archived).join("foreign");
    fs::write(&residue, b"inexact projected wrapper").unwrap();

    let prepared = fixture.prepare(&[head_state(), archived]).unwrap();
    assert_eq!(prepared.eligible_state_ids(), &[head_state()]);
    assert_eq!(prepared.exclusions().len(), 1);
    assert_eq!(prepared.exclusions()[0].state_id(), archived);
    assert_eq!(
        prepared.exclusions()[0].reason(),
        ArchivedBootStateRootExclusionReason::WrapperLayoutInexact
    );

    fs::remove_file(residue).unwrap();
    let revalidated = prepared.revalidate(&fixture.installation).unwrap();
    assert_eq!(prepared.eligible_state_ids(), &[head_state()]);
    assert_eq!(revalidated.roots().count(), 1);
}

#[test]
fn admitted_archive_substitution_is_a_hard_revalidation_failure() {
    let fixture = Fixture::new();
    let archived = state(83);
    fixture.create_archive(archived);
    let prepared = fixture.prepare(&[head_state(), archived]).unwrap();

    let canonical = fixture.archive_path(archived);
    let saved = fixture.installation.root_path("saved-admitted-archive");
    fs::rename(&canonical, &saved).unwrap();
    fixture.create_archive(archived);

    assert!(matches!(
        prepared.revalidate(&fixture.installation),
        Err(ActiveReblitBootStateRootsError::ArchivedChanged { state, .. })
            if state == i32::from(archived)
    ));
}

#[test]
fn archive_substitution_between_global_revalidation_passes_is_caught() {
    let fixture = Fixture::new();
    let archived = state(87);
    fixture.create_archive(archived);
    let prepared = fixture.prepare(&[head_state(), archived]).unwrap();

    let canonical = fixture.archive_path(archived);
    let saved = fixture.installation.root_path("saved-between-pass-archive");
    arm_between_revalidation_passes(move || {
        fs::rename(&canonical, &saved).unwrap();
        fs::create_dir(&canonical).unwrap();
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o700)).unwrap();
        create_exact_tree(&canonical.join("usr"), archived);
    });

    assert!(matches!(
        prepared.revalidate(&fixture.installation),
        Err(ActiveReblitBootStateRootsError::ArchivedChanged { state, .. })
            if state == i32::from(archived)
    ));
}

#[test]
fn intermediate_permission_failure_is_never_downgraded_to_an_archive_exclusion() {
    let fixture = Fixture::new();
    let archived = state(84);
    fixture.create_archive(archived);
    let cast = fixture.installation.root.join(".cast");
    let original_mode = fs::symlink_metadata(&cast).unwrap().permissions().mode() & 0o7777;
    fs::set_permissions(&cast, fs::Permissions::from_mode(0o000)).unwrap();

    let result = fixture.prepare(&[head_state(), archived]);
    fs::set_permissions(&cast, fs::Permissions::from_mode(original_mode)).unwrap();

    assert!(matches!(result, Err(ActiveReblitBootStateRootsError::Roots { .. })));
}
