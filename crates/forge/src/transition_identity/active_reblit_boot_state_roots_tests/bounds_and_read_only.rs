use std::{
    ffi::OsString,
    fs,
    os::{fd::AsRawFd as _, unix::ffi::OsStringExt as _},
    path::PathBuf,
    time::Duration,
};

use super::*;

#[test]
fn projection_count_head_order_duplicate_and_positive_id_bounds_are_typed() {
    let head = head_state();
    assert!(matches!(
        validate_projection(head, &[]),
        Err(ActiveReblitBootStateRootsError::StateCount { actual: 0, limit })
            if limit == MAX_ACTIVE_REBLIT_BOOT_STATE_ROOTS
    ));

    let oversized = [head, state(1), state(2), state(3), state(4), state(5)];
    assert!(matches!(
        validate_projection(head, &oversized),
        Err(ActiveReblitBootStateRootsError::StateCount { actual, limit })
            if actual == oversized.len() && limit == MAX_ACTIVE_REBLIT_BOOT_STATE_ROOTS
    ));
    assert!(matches!(
        validate_projection(head, &[state(1), head]),
        Err(ActiveReblitBootStateRootsError::HeadOrder { expected, actual: Some(1) })
            if expected == i32::from(head)
    ));
    assert!(matches!(
        validate_projection(head, &[head, head]),
        Err(ActiveReblitBootStateRootsError::DuplicateState { state })
            if state == i32::from(head)
    ));
    assert!(matches!(
        validate_projection(head, &[head, state(0)]),
        Err(ActiveReblitBootStateRootsError::NonPositiveState { state: 0 })
    ));
}

#[test]
fn diagnostic_path_byte_and_component_bounds_admit_n_and_reject_n_plus_one() {
    let byte_limit = PathBuf::from(OsString::from_vec(vec![b'a'; MAX_STATE_ROOT_PATH_BYTES]));
    let byte_over = PathBuf::from(OsString::from_vec(vec![b'a'; MAX_STATE_ROOT_PATH_BYTES + 1]));
    assert!(validate_diagnostic_path(&byte_limit).is_ok());
    assert!(matches!(
        validate_diagnostic_path(&byte_over),
        Err(ActiveReblitBootStateRootsError::PathBytes { actual, limit, .. })
            if actual == MAX_STATE_ROOT_PATH_BYTES + 1 && limit == MAX_STATE_ROOT_PATH_BYTES
    ));

    let component_limit = (0..MAX_STATE_ROOT_PATH_COMPONENTS).fold(PathBuf::new(), |mut path, _| {
        path.push("a");
        path
    });
    let component_over = component_limit.join("a");
    assert!(validate_diagnostic_path(&component_limit).is_ok());
    assert!(matches!(
        validate_diagnostic_path(&component_over),
        Err(ActiveReblitBootStateRootsError::PathComponents { actual, limit, .. })
            if actual == MAX_STATE_ROOT_PATH_COMPONENTS + 1 && limit == MAX_STATE_ROOT_PATH_COMPONENTS
    ));
}

#[test]
fn exact_head_work_boundary_admits_n_and_rejects_n_minus_one() {
    let fixture = Fixture::new();
    let exact_head_work = 22;
    fixture
        .prepare_with_policy(&[head_state()], exact_head_work, Duration::from_secs(30))
        .unwrap();
    assert!(matches!(
        fixture.prepare_with_policy(&[head_state()], exact_head_work - 1, Duration::from_secs(30)),
        Err(ActiveReblitBootStateRootsError::WorkLimit { limit, .. })
            if limit == exact_head_work - 1
    ));
}

#[test]
fn expired_deadline_fails_before_state_root_admission() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture.prepare_with_policy(&[head_state()], usize::MAX, Duration::ZERO),
        Err(ActiveReblitBootStateRootsError::Deadline { .. })
    ));
}

#[test]
fn preparation_revalidation_and_bound_views_are_read_only() {
    let fixture = Fixture::new();
    let archived = state(85);
    let token = fixture.create_archive(archived);
    let wrapper = fixture.archive_path(archived);
    fs::hard_link(
        wrapper.join("usr/.cast-tree-id"),
        wrapper.join(format!(".cast-state-slot-{archived}-{token}")),
    )
    .unwrap();
    let before = namespace_snapshot(&fixture.installation.root);

    let prepared = fixture.prepare(&[head_state(), archived]).unwrap();
    let revalidated = prepared.revalidate(&fixture.installation).unwrap();
    for root in revalidated.roots() {
        let flags = unsafe { nix::libc::fcntl(root.usr().as_raw_fd(), nix::libc::F_GETFL) };
        assert_ne!(flags, -1);
        assert_eq!(flags & nix::libc::O_ACCMODE, nix::libc::O_RDONLY);
    }
    drop(revalidated);
    let _revalidated = prepared.revalidate(&fixture.installation).unwrap();

    assert_eq!(namespace_snapshot(&fixture.installation.root), before);
}
