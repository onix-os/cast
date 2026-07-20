use std::{fs, os::unix::fs::MetadataExt as _, path::PathBuf};

use crate::{
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{Phase, RecoveryDisposition, RollbackActionOutcome},
};

use super::fixture::{Fixture, OperationKind, SourceCase, pending};

#[test]
fn startup_root_links_complete_fresh_entries_reach_usr_restored_once_then_remain_byte_stable() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            let fixture = if historical {
                Fixture::historical(kind, SourceCase::RootLinksCompletePost)
            } else {
                Fixture::new(kind, SourceCase::RootLinksCompletePost)
            };
            let case = format!("{kind:?} historical={historical}");
            fixture.assert_source_unchanged();
            let namespace_before = fixture.namespace_snapshot();
            let database_before = fixture.database_snapshot();
            let root_links_before = root_link_snapshot(&fixture);
            let usr_before = usr_layout(&fixture);
            reset_retained_exchange_syscall_count();

            let decision_entry = fixture.enter();
            assert_eq!(pending(&decision_entry).phase(), Phase::RollbackDecided, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 0, "{case}");
            drop(decision_entry);
            let decision = fixture.canonical_record();
            fixture.assert_exact_decision(&decision);
            assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");

            let route_entry = fixture.enter();
            let reverse_intent = decision.rollback_successor(None).unwrap();
            assert_eq!(reverse_intent.phase, Phase::ReverseExchangeIntent, "{case}");
            assert_eq!(pending(&route_entry).phase(), Phase::ReverseExchangeIntent, "{case}");
            assert_eq!(
                pending(&route_entry).disposition(),
                RecoveryDisposition::ResumeRollback {
                    phase: Phase::ReverseExchangeIntent,
                },
                "{case}"
            );
            assert!(pending(&route_entry).blockers().is_empty(), "{case}");
            assert_eq!(fixture.canonical_record(), reverse_intent, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 0, "{case}");
            assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");

            let routed_bytes = fixture.canonical_bytes();
            drop(route_entry);
            let reverse_entry = fixture.enter();
            let restored = reverse_intent
                .rollback_successor(Some(RollbackActionOutcome::Applied))
                .unwrap();
            assert_eq!(restored.phase, Phase::UsrRestored, "{case}");
            assert_eq!(pending(&reverse_entry).phase(), Phase::UsrRestored, "{case}");
            assert_eq!(
                pending(&reverse_entry).disposition(),
                RecoveryDisposition::ResumeRollback {
                    phase: Phase::UsrRestored,
                },
                "{case}"
            );
            assert!(pending(&reverse_entry).blockers().is_empty(), "{case}");
            assert_eq!(fixture.canonical_record(), restored, "{case}");
            assert_ne!(fixture.canonical_bytes(), routed_bytes, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");
            assert_layout_reversed(usr_before, usr_layout(&fixture), &case);
            assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");

            let restored_bytes = fixture.canonical_bytes();
            let usr_restored = usr_layout(&fixture);
            drop(reverse_entry);
            let stable_entry = fixture.enter();
            assert_eq!(pending(&stable_entry).phase(), Phase::UsrRestored, "{case}");
            assert_eq!(fixture.canonical_record(), restored, "{case}");
            assert_eq!(fixture.canonical_bytes(), restored_bytes, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");
            assert_eq!(usr_layout(&fixture), usr_restored, "{case}");
            assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RootLinkIdentity {
    name: &'static str,
    target: PathBuf,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
}

fn root_link_snapshot(fixture: &Fixture) -> Vec<RootLinkIdentity> {
    fixture.assert_complete_root_abi();
    ["bin", "sbin", "lib", "lib32", "lib64"]
        .into_iter()
        .map(|name| {
            let path = fixture.installation.root.join(name);
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(metadata.file_type().is_symlink());
            RootLinkIdentity {
                name,
                target: fs::read_link(path).unwrap(),
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                links: metadata.nlink(),
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UsrLayout {
    live: (u64, u64),
    staged: (u64, u64),
}

fn usr_layout(fixture: &Fixture) -> UsrLayout {
    UsrLayout {
        live: directory_identity(&fixture.installation.root.join("usr")),
        staged: directory_identity(&fixture.installation.root.join(".cast/root/staging/usr")),
    }
}

fn directory_identity(path: &std::path::Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_dir());
    (metadata.dev(), metadata.ino())
}

fn assert_layout_reversed(before: UsrLayout, after: UsrLayout, case: &str) {
    assert_eq!(after.live, before.staged, "{case}");
    assert_eq!(after.staged, before.live, "{case}");
}
