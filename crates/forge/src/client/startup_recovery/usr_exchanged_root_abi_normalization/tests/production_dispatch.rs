use std::{fs, os::unix::fs::symlink};

use crate::{
    client::startup_reconciliation::{
        reset_usr_exchanged_root_abi_effect_counts, usr_exchanged_root_abi_complete_sync_attempts,
        usr_exchanged_root_abi_publication_attempts,
    },
    transition_journal::Phase,
};

use super::fixture::{Fixture, OperationKind, SourceCase, pending};

#[test]
fn startup_usr_exchanged_root_abi_temporary_and_foreign_final_names_never_mutate_or_decide() {
    let temporary = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    temporary.set_root_abi_subset(0);
    let source = temporary.canonical_bytes();
    symlink("usr/bin", temporary.installation.root.join("bin.next")).unwrap();
    reset_usr_exchanged_root_abi_effect_counts();
    let error = temporary.enter();
    assert_eq!(pending(&error).phase(), Phase::UsrExchanged);
    assert_eq!(temporary.canonical_bytes(), source);
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 0);
    assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 0);
    assert_eq!(fs::read_link(temporary.installation.root.join("bin.next")).unwrap(), std::path::Path::new("usr/bin"));

    let foreign = Fixture::new(OperationKind::Archived, SourceCase::ExchangedPost);
    foreign.set_root_abi_subset(0);
    let source = foreign.canonical_bytes();
    symlink("usr/not-bin", foreign.installation.root.join("bin")).unwrap();
    reset_usr_exchanged_root_abi_effect_counts();
    let error = foreign.enter();
    assert_eq!(pending(&error).phase(), Phase::UsrExchanged);
    assert_eq!(foreign.canonical_bytes(), source);
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 0);
    assert_eq!(fs::read_link(foreign.installation.root.join("bin")).unwrap(), std::path::Path::new("usr/not-bin"));
}

#[test]
fn startup_usr_exchanged_root_abi_incomplete_success_ends_one_entry_at_source() {
    let fixture = Fixture::new(OperationKind::ActiveReblit, SourceCase::ExchangedPost);
    fixture.set_root_abi_subset(0b10101);
    let source = fixture.canonical_bytes();
    let database = fixture.database_snapshot();
    reset_usr_exchanged_root_abi_effect_counts();

    let error = fixture.enter();
    assert_eq!(pending(&error).phase(), Phase::UsrExchanged);
    assert_eq!(fixture.canonical_bytes(), source);
    assert_eq!(fixture.database_snapshot(), database);
    fixture.assert_complete_root_abi();
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
    assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 0);
}

#[test]
fn startup_root_abi_normalizer_is_sealed_from_non_usr_exchanged_sources() {
    let fixture = Fixture::new(OperationKind::NewState, SourceCase::IntentPre);
    reset_usr_exchanged_root_abi_effect_counts();
    let error = fixture.enter();
    assert_eq!(pending(&error).phase(), Phase::RollbackDecided);
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 0);
    assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 0);
}
