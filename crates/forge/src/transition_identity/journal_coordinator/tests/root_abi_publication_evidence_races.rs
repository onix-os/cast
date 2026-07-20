#[test]
fn journal_coordinator_root_links_complete_new_state_post_publication_database_and_provenance_races_fail_stop() {
    for mutation in ["ownership", "provenance", "metadata", "state-id"] {
        let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(CandidateKind::NewState, 0);
        let source = exchanged.record().clone();
        let database = fixture.database.clone();
        let candidate = fixture.candidate_state;
        let transition = source.transition_id.clone();
        let live_usr = fixture.installation.root.join("usr");
        crate::client::arm_before_retained_root_abi_link_publication(4, move || match mutation {
            "ownership" => database.clear_transition_if_matches(candidate, &transition).unwrap(),
            "provenance" => database.delete_metadata_provenance_for_test(candidate).unwrap(),
            "metadata" => replace_file_with_same_bytes(
                &live_usr.join("lib/os-release"),
                "os-release.root-links-displaced",
            ),
            "state-id" => replace_file_with_same_bytes(
                &live_usr.join(".stateID"),
                ".stateID.root-links-displaced",
            ),
            _ => unreachable!(),
        });

        let failure = exchanged.publish_root_abi().unwrap_err();

        crate::client::assert_before_retained_root_abi_link_publication_consumed();
        assert!(
            matches!(failure, RootAbiPublicationFailure::PostEffectEvidence { .. }),
            "mutation={mutation} failure={failure:?}"
        );
        assert_usr_exchanged_source(&fixture, &source);
        assert_root_links_complete(&fixture);
    }
}

#[test]
fn journal_coordinator_root_links_complete_archived_post_publication_database_and_metadata_races_fail_stop() {
    for mutation in ["candidate", "previous", "provenance", "metadata"] {
        let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(CandidateKind::Archived, 0);
        let source = exchanged.record().clone();
        let database = fixture.database.clone();
        let candidate = fixture.candidate_state;
        let previous = fixture.previous_state;
        let live_usr = fixture.installation.root.join("usr");
        crate::client::arm_before_retained_root_abi_link_publication(4, move || match mutation {
            "candidate" => database.remove(&candidate).unwrap(),
            "previous" => database.remove(&previous).unwrap(),
            "provenance" => database.delete_metadata_provenance_for_test(candidate).unwrap(),
            "metadata" => replace_file_with_same_bytes(
                &live_usr.join("lib/system-model.glu"),
                "system-model.glu.root-links-displaced",
            ),
            _ => unreachable!(),
        });

        let failure = exchanged.publish_root_abi().unwrap_err();

        crate::client::assert_before_retained_root_abi_link_publication_consumed();
        assert!(
            matches!(failure, RootAbiPublicationFailure::PostEffectEvidence { .. }),
            "mutation={mutation} failure={failure:?}"
        );
        assert_usr_exchanged_source(&fixture, &source);
        assert_root_links_complete(&fixture);
    }
}

#[test]
fn journal_coordinator_root_links_complete_active_reblit_post_publication_state_and_reservation_races_fail_stop() {
    for mutation in ["state", "provenance", "parked-slot", "replacement-reservation"] {
        let (fixture, identity, authority) = fixture_parts_with_root_abi_mask(
            CandidateKind::ActiveReblit,
            PreviousKind::Active,
            true,
            true,
            0,
        );
        let authority = authority.expect("ActiveReblit evidence-race authority");
        let (fixture, intent, authority) =
            coordinator_from_exchange_fixture(CandidateKind::ActiveReblit, fixture, identity, authority);
        let exchanged = intent.execute_usr_exchange(authority).unwrap();
        let source = exchanged.record().clone();
        let database = fixture.database.clone();
        let candidate = fixture.candidate_state;
        let parked = active_reblit_parked_slot_path(&fixture, &source, 0);
        let replacement = active_reblit_replacement_path(&fixture, &source, 0);
        let parked_displaced = fixture.installation.root_path("root-links-parked.displaced");
        let replacement_displaced = fixture
            .installation
            .state_quarantine_dir()
            .join("root-links-reservation.displaced");
        crate::client::arm_before_retained_root_abi_link_publication(4, move || match mutation {
            "state" => database.remove(&candidate).unwrap(),
            "provenance" => database.delete_metadata_provenance_for_test(candidate).unwrap(),
            "parked-slot" => {
                fs::rename(&parked, &parked_displaced).unwrap();
                fs::create_dir(&parked).unwrap();
                fs::set_permissions(&parked, fs::Permissions::from_mode(0o700)).unwrap();
            }
            "replacement-reservation" => {
                fs::rename(&replacement, &replacement_displaced).unwrap();
                fs::create_dir(&replacement).unwrap();
                fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
            }
            _ => unreachable!(),
        });

        let failure = exchanged.publish_root_abi().unwrap_err();

        crate::client::assert_before_retained_root_abi_link_publication_consumed();
        assert!(
            matches!(failure, RootAbiPublicationFailure::PostEffectEvidence { .. }),
            "mutation={mutation} failure={failure:?}"
        );
        assert_usr_exchanged_source(&fixture, &source);
        assert_root_links_complete(&fixture);
    }
}

#[test]
fn journal_coordinator_root_links_complete_retained_namespace_binding_races_fail_stop() {
    for mutation in ["root", "cast", "journal", "lock"] {
        let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(CandidateKind::Archived, 0);
        let source = exchanged.record().clone();
        let root = fixture.installation.root.clone();
        let root_displaced = root.with_extension(format!("root-links-{mutation}-displaced"));
        let cast = root.join(".cast");
        let cast_displaced = root.join(".cast.root-links-displaced");
        let journal = cast.join("journal");
        let journal_displaced = cast.join("journal.root-links-displaced");
        let lock = journal.join("state-transition.lock");
        let lock_displaced = journal.join("state-transition.lock.root-links-displaced");
        let hook_root = root.clone();
        let hook_root_displaced = root_displaced.clone();
        let hook_cast = cast.clone();
        let hook_cast_displaced = cast_displaced.clone();
        let hook_journal = journal.clone();
        let hook_journal_displaced = journal_displaced.clone();
        let hook_lock = lock.clone();
        let hook_lock_displaced = lock_displaced.clone();
        crate::client::arm_before_retained_root_abi_link_publication(0, move || match mutation {
            "root" => {
                fs::rename(&hook_root, &hook_root_displaced).unwrap();
                fs::create_dir(&hook_root).unwrap();
                fs::set_permissions(&hook_root, fs::Permissions::from_mode(0o755)).unwrap();
            }
            "cast" => {
                fs::rename(&hook_cast, &hook_cast_displaced).unwrap();
                fs::create_dir(&hook_cast).unwrap();
                fs::set_permissions(&hook_cast, fs::Permissions::from_mode(0o700)).unwrap();
            }
            "journal" => {
                fs::rename(&hook_journal, &hook_journal_displaced).unwrap();
                fs::create_dir(&hook_journal).unwrap();
                fs::set_permissions(&hook_journal, fs::Permissions::from_mode(0o700)).unwrap();
            }
            "lock" => {
                fs::rename(&hook_lock, &hook_lock_displaced).unwrap();
                fs::write(&hook_lock, b"replacement journal lock").unwrap();
                fs::set_permissions(&hook_lock, fs::Permissions::from_mode(0o600)).unwrap();
            }
            _ => unreachable!(),
        });

        let failure = exchanged.publish_root_abi().unwrap_err();

        crate::client::assert_before_retained_root_abi_link_publication_consumed();
        assert_root_abi_publication_failure(&failure);
        match mutation {
            "root" => {
                assert_eq!(
                    decode(&fs::read(canonical_journal(&root_displaced)).unwrap()).unwrap(),
                    source
                );
                assert_state_metadata_name_absent(&root.join("bin"));
                fs::remove_dir(&root).unwrap();
                fs::rename(&root_displaced, &root).unwrap();
            }
            "cast" => assert_eq!(
                decode(&fs::read(cast_displaced.join("journal/state-transition")).unwrap()).unwrap(),
                source
            ),
            "journal" => assert_eq!(
                decode(&fs::read(journal_displaced.join("state-transition")).unwrap()).unwrap(),
                source
            ),
            "lock" => assert_usr_exchanged_source(&fixture, &source),
            _ => unreachable!(),
        }
    }
}

#[test]
fn journal_coordinator_root_links_complete_same_byte_canonical_journal_inode_replacement_fails_stop() {
    let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(CandidateKind::NewState, 0);
    let source = exchanged.record().clone();
    let canonical = canonical_journal(&fixture.installation.root);
    let displaced = fixture.installation.root.join("state-transition.root-links-displaced");
    let hook_canonical = canonical.clone();
    let hook_displaced = displaced.clone();
    crate::client::arm_before_retained_root_abi_link_publication(0, move || {
        replace_regular_file_with_same_bytes_at(&hook_canonical, &hook_displaced);
    });

    let failure = exchanged.publish_root_abi().unwrap_err();

    crate::client::assert_before_retained_root_abi_link_publication_consumed();
    assert!(matches!(
        failure,
        RootAbiPublicationFailure::CompletionPersistence { .. }
    ), "unexpected same-byte journal replacement failure: {failure:?}");
    assert_eq!(decode(&fs::read(&canonical).unwrap()).unwrap(), source);
    assert_eq!(decode(&fs::read(&displaced).unwrap()).unwrap(), source);
    assert_ne!(
        (fs::metadata(&canonical).unwrap().dev(), fs::metadata(&canonical).unwrap().ino()),
        (fs::metadata(&displaced).unwrap().dev(), fs::metadata(&displaced).unwrap().ino())
    );
    assert_root_links_complete(&fixture);
}
