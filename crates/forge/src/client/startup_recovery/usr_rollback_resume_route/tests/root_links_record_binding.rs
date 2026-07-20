use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
};

use crate::client::{
    startup_gate,
    startup_recovery::{
        DurableUsrRollbackResumeRouteRecord, UsrRollbackResumeRoutePersistenceError,
        arm_before_usr_rollback_resume_route_final_revalidation,
    },
};

use super::{
    super::{
        UsrRollbackResumeRouteSuccessorBindingError,
        arm_after_usr_rollback_resume_route_successor_binding_check_before_reopen,
        arm_before_usr_rollback_resume_route_successor_binding_revalidation,
    },
    fixture::{OperationKind, SourceCase, canonical_journal},
    support::RouteFixture,
};

#[test]
fn startup_root_links_complete_route_same_byte_predecessor_replacement_breaks_exact_binding() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            let fixture = if historical {
                RouteFixture::historical(kind, SourceCase::RootLinksCompletePost)
            } else {
                RouteFixture::new(kind, SourceCase::RootLinksCompletePost)
            };
            let canonical = canonical_journal(&fixture.fixture.installation.root);
            let displaced = fixture
                .fixture
                .installation
                .root
                .join("root-links-route-predecessor-displaced");
            let before = fixture.fixture.canonical_bytes();
            let hook_canonical = canonical.clone();
            let hook_displaced = displaced.clone();
            let hook_bytes = before.clone();
            arm_before_usr_rollback_resume_route_final_revalidation(move || {
                fs::rename(&hook_canonical, &hook_displaced).unwrap();
                fs::write(&hook_canonical, hook_bytes).unwrap();
                fs::set_permissions(&hook_canonical, fs::Permissions::from_mode(0o600)).unwrap();
            });

            let error = fixture.enter();

            assert!(
                matches!(
                    error,
                    startup_gate::Error::UsrRollbackResumeRoutePersistence(
                        UsrRollbackResumeRoutePersistenceError::Authority(_)
                    )
                ),
                "{kind:?} historical={historical}: {error:?}"
            );
            assert_eq!(fixture.fixture.canonical_bytes(), before, "{kind:?} historical={historical}");
            assert_eq!(fs::read(&displaced).unwrap(), before, "{kind:?} historical={historical}");
            let retained = fs::symlink_metadata(&displaced).unwrap();
            let replacement = fs::symlink_metadata(&canonical).unwrap();
            assert_ne!(
                (retained.dev(), retained.ino()),
                (replacement.dev(), replacement.ino()),
                "{kind:?} historical={historical}"
            );
        }
    }
}

#[test]
fn startup_root_links_complete_route_same_byte_successor_replacement_reopens_but_never_succeeds() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            let fixture = if historical {
                RouteFixture::historical(kind, SourceCase::RootLinksCompletePost)
            } else {
                RouteFixture::new(kind, SourceCase::RootLinksCompletePost)
            };
            let canonical = canonical_journal(&fixture.fixture.installation.root);
            let displaced = fixture
                .fixture
                .installation
                .root
                .join("root-links-route-successor-displaced");
            let hook_canonical = canonical.clone();
            let hook_displaced = displaced.clone();
            arm_before_usr_rollback_resume_route_successor_binding_revalidation(move || {
                let bytes = fs::read(&hook_canonical).unwrap();
                fs::rename(&hook_canonical, &hook_displaced).unwrap();
                fs::write(&hook_canonical, bytes).unwrap();
                fs::set_permissions(&hook_canonical, fs::Permissions::from_mode(0o600)).unwrap();
            });

            let error = fixture.enter();

            assert!(
                matches!(
                    error,
                    startup_gate::Error::UsrRollbackResumeRoutePersistence(
                        UsrRollbackResumeRoutePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackResumeRouteRecord::Successor,
                            source: UsrRollbackResumeRouteSuccessorBindingError::Changed,
                        }
                    )
                ),
                "{kind:?} historical={historical}: {error:?}"
            );
            let successor = fixture.canonical_record();
            fixture.assert_exact_route(&successor);
            assert_eq!(
                fs::read(&displaced).unwrap(),
                fixture.fixture.canonical_bytes(),
                "{kind:?} historical={historical}"
            );
            let retained = fs::symlink_metadata(&displaced).unwrap();
            let replacement = fs::symlink_metadata(&canonical).unwrap();
            assert_ne!(
                (retained.dev(), retained.ino()),
                (replacement.dev(), replacement.ino()),
                "{kind:?} historical={historical}"
            );
        }
    }
}

#[test]
fn startup_root_links_complete_route_same_byte_successor_replacement_after_binding_before_reopen_never_succeeds() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            let fixture = if historical {
                RouteFixture::historical(kind, SourceCase::RootLinksCompletePost)
            } else {
                RouteFixture::new(kind, SourceCase::RootLinksCompletePost)
            };
            let canonical = canonical_journal(&fixture.fixture.installation.root);
            let displaced = fixture
                .fixture
                .installation
                .root
                .join("root-links-route-bound-successor-displaced");
            let hook_canonical = canonical.clone();
            let hook_displaced = displaced.clone();
            arm_after_usr_rollback_resume_route_successor_binding_check_before_reopen(move || {
                let bytes = fs::read(&hook_canonical).unwrap();
                fs::rename(&hook_canonical, &hook_displaced).unwrap();
                fs::write(&hook_canonical, bytes).unwrap();
                fs::set_permissions(&hook_canonical, fs::Permissions::from_mode(0o600)).unwrap();
            });

            let error = fixture.enter();

            assert!(
                matches!(
                    error,
                    startup_gate::Error::UsrRollbackResumeRoutePersistence(
                        UsrRollbackResumeRoutePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackResumeRouteRecord::Successor,
                            source: UsrRollbackResumeRouteSuccessorBindingError::Changed,
                        }
                    )
                ),
                "{kind:?} historical={historical}: {error:?}"
            );
            let successor = fixture.canonical_record();
            fixture.assert_exact_route(&successor);
            assert_eq!(
                fs::read(&displaced).unwrap(),
                fixture.fixture.canonical_bytes(),
                "{kind:?} historical={historical}"
            );
            let retained = fs::symlink_metadata(&displaced).unwrap();
            let replacement = fs::symlink_metadata(&canonical).unwrap();
            assert_ne!(
                (retained.dev(), retained.ino()),
                (replacement.dev(), replacement.ino()),
                "{kind:?} historical={historical}"
            );
        }
    }
}
