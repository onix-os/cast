use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use crate::{
    client::{
        startup_gate,
        startup_reconciliation::arm_before_usr_rollback_reverse_fresh_namespace_capture,
        startup_recovery::{
            UsrRollbackReverseDispatchError, UsrRollbackReversePersistenceError,
            arm_before_usr_rollback_reverse_persistence_final_revalidation,
        },
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::RollbackActionOutcome,
};

use super::support::{
    Fixture, OperationKind, ReverseLayout, SourceCase, assert_layout_reversed, assert_layout_unchanged,
    assert_usr_restored_pending, enter, expected_usr_restored, usr_layout,
};

fn canonical_journal(fixture: &Fixture) -> PathBuf {
    fixture
        .fixture
        .installation
        .root
        .join(".cast/journal/state-transition")
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn same_byte_replacement_hook(
    fixture: &Fixture,
    displaced_name: String,
) -> ((u64, u64), PathBuf, impl FnOnce() + 'static) {
    let canonical = canonical_journal(fixture);
    let retained_identity = inode_identity(&canonical);
    let displaced = fixture.fixture.installation.root.join(displaced_name);
    let hook_canonical = canonical;
    let hook_displaced = displaced.clone();
    let bytes = fixture.fixture.canonical_bytes();
    let hook = move || {
        fs::rename(&hook_canonical, &hook_displaced).unwrap();
        fs::write(&hook_canonical, bytes).unwrap();
        fs::set_permissions(&hook_canonical, fs::Permissions::from_mode(0o600)).unwrap();
    };
    (retained_identity, displaced, hook)
}

fn assert_same_byte_replacement(
    fixture: &Fixture,
    retained_identity: (u64, u64),
    displaced: &Path,
    expected_bytes: &[u8],
) {
    assert_eq!(fs::read(displaced).unwrap(), expected_bytes);
    assert_eq!(fixture.fixture.canonical_bytes(), expected_bytes);
    assert_eq!(inode_identity(displaced), retained_identity);
    assert_ne!(inode_identity(&canonical_journal(fixture)), retained_identity);
}

fn assert_dispatch_authority(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackReverseDispatch(UsrRollbackReverseDispatchError::Authority(_))
        ),
        "expected exact reverse-dispatch authority failure, got {error:?}"
    );
}

fn assert_persistence_authority(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackReverseDispatch(UsrRollbackReverseDispatchError::Persistence(
                UsrRollbackReversePersistenceError::Authority(_)
            ))
        ),
        "expected exact reverse-persistence authority failure, got {error:?}"
    );
}

#[test]
fn startup_root_links_reverse_same_byte_predecessor_replacement_at_pre_effect_boundary_never_authorizes_effect() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            for layout in [ReverseLayout::Post, ReverseLayout::Pre] {
                let fixture = Fixture::for_effect_source(
                    kind,
                    SourceCase::RootLinksCompletePost,
                    layout,
                    historical,
                );
                let case = format!("{kind:?} {layout:?} historical={historical}");
                let source_bytes = fixture.fixture.canonical_bytes();
                let database_before = fixture.fixture.database_snapshot();
                let root_abi_before = fixture.root_abi_snapshot();
                let usr_before = usr_layout(&fixture);
                let (retained_identity, displaced, hook) = same_byte_replacement_hook(
                    &fixture,
                    format!("root-links-reverse-pre-effect-{kind:?}-{layout:?}-{historical}"),
                );
                arm_before_usr_rollback_reverse_fresh_namespace_capture(hook);
                reset_retained_exchange_syscall_count();

                let error = enter(&fixture);

                assert_dispatch_authority(&error);
                assert_eq!(retained_exchange_syscall_count(), 0, "{case}");
                assert_layout_unchanged(usr_before, usr_layout(&fixture));
                assert_eq!(fixture.fixture.database_snapshot(), database_before, "{case}");
                assert_same_byte_replacement(&fixture, retained_identity, &displaced, &source_bytes);
                fixture.assert_root_abi_unchanged(&root_abi_before);
                drop(error);

                let restarted = enter(&fixture);
                let outcome = match layout {
                    ReverseLayout::Post => RollbackActionOutcome::Applied,
                    ReverseLayout::Pre => RollbackActionOutcome::AlreadySatisfied,
                };
                assert_usr_restored_pending(&restarted);
                assert_eq!(fixture.fixture.canonical_record(), expected_usr_restored(&fixture, outcome), "{case}");
                assert_eq!(
                    retained_exchange_syscall_count(),
                    usize::from(layout == ReverseLayout::Post),
                    "{case}"
                );
                fixture.assert_root_abi_unchanged(&root_abi_before);
            }
        }
    }
}

#[test]
fn startup_root_links_reverse_same_byte_predecessor_replacement_after_exchange_before_persistence_never_advances() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            let fixture = Fixture::for_effect_source(
                kind,
                SourceCase::RootLinksCompletePost,
                ReverseLayout::Post,
                historical,
            );
            let case = format!("{kind:?} historical={historical}");
            let source_bytes = fixture.fixture.canonical_bytes();
            let database_before = fixture.fixture.database_snapshot();
            let root_abi_before = fixture.root_abi_snapshot();
            let usr_before = usr_layout(&fixture);
            let (retained_identity, displaced, hook) = same_byte_replacement_hook(
                &fixture,
                format!("root-links-reverse-before-persistence-{kind:?}-{historical}"),
            );
            arm_before_usr_rollback_reverse_persistence_final_revalidation(hook);
            reset_retained_exchange_syscall_count();

            let error = enter(&fixture);

            assert_persistence_authority(&error);
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            assert_layout_reversed(usr_before, usr_layout(&fixture));
            assert_eq!(fixture.fixture.database_snapshot(), database_before, "{case}");
            assert_same_byte_replacement(&fixture, retained_identity, &displaced, &source_bytes);
            fixture.assert_root_abi_unchanged(&root_abi_before);
            drop(error);

            let restarted = enter(&fixture);
            assert_usr_restored_pending(&restarted);
            assert_eq!(
                fixture.fixture.canonical_record(),
                expected_usr_restored(&fixture, RollbackActionOutcome::AlreadySatisfied),
                "{case}"
            );
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            fixture.assert_root_abi_unchanged(&root_abi_before);
        }
    }
}
