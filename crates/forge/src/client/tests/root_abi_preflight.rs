use std::{cell::Cell, rc::Rc};

use super::*;

#[test]
fn every_live_root_abi_conflict_precedes_candidate_trigger_and_exchange_mutation() {
    for (expected_target, target) in ROOT_ABI_LINKS {
        for staging_name in [false, true] {
            let fixture = stateful_transition_fixture(false);
            let installation = &fixture.client.installation;
            let live_usr = installation.root.join("usr");
            let candidate_usr = installation.staging_path("usr");
            let live_usr_identity = root_abi_inode(&live_usr);
            let candidate_usr_identity = root_abi_inode(&candidate_usr);
            let staging_identity = root_abi_inode(&installation.staging_dir());
            let states = fixture.client.state_db.all().unwrap();
            let conflict = installation.root.join(if staging_name {
                format!("{target}.next")
            } else {
                target.to_owned()
            });
            let payload = format!("foreign root ABI entry at {}", conflict.display());
            fs::write(&conflict, payload.as_bytes()).unwrap();
            let foreign_identity = root_abi_inode(&conflict);
            assert!(take_observed_trigger_scopes().is_empty());

            let mut checkpoints = Vec::new();
            let error = fixture
                .client
                .apply_stateful_blit_with_checkpoint(
                    vfs(Vec::new()).unwrap(),
                    &fixture.candidate,
                    Some(fixture.previous.id),
                    generated_system_snapshot("candidate-package"),
                    |checkpoint| {
                        checkpoints.push(checkpoint);
                        Ok(())
                    },
                )
                .unwrap_err();

            let exact_conflict = if staging_name {
                matches!(
                    &error,
                    Error::RootAbiStagingConflict {
                        path,
                        actual_type: "regular file",
                        symlink_target: None,
                    } if path == &conflict
                )
            } else {
                matches!(
                    &error,
                    Error::RootAbiLinkTypeConflict {
                        path,
                        target: found_target,
                        actual_type: "regular file",
                    } if path == &conflict && found_target == expected_target
                )
            };
            assert!(
                exact_conflict,
                "unexpected root ABI conflict for {conflict:?}: {error:#?}"
            );
            assert!(
                checkpoints.is_empty(),
                "root ABI preflight reached a transition checkpoint"
            );
            assert!(
                take_observed_trigger_scopes().is_empty(),
                "root ABI preflight reached a trigger boundary"
            );

            assert_eq!(root_abi_inode(&conflict), foreign_identity);
            assert_eq!(fs::read(&conflict).unwrap(), payload.as_bytes());
            for (_, other) in ROOT_ABI_LINKS {
                let final_name = installation.root.join(other);
                if final_name != conflict {
                    assert_root_abi_absent(&final_name);
                }
                let next_name = installation.root.join(format!("{other}.next"));
                if next_name != conflict {
                    assert_root_abi_absent(&next_name);
                }
            }

            assert_eq!(root_abi_inode(&live_usr), live_usr_identity);
            assert_eq!(root_abi_inode(&candidate_usr), candidate_usr_identity);
            assert_eq!(root_abi_inode(&installation.staging_dir()), staging_identity);
            assert_eq!(
                fs::read_to_string(live_usr.join(".stateID")).unwrap(),
                fixture.previous.id.to_string()
            );
            assert_eq!(
                fs::read_to_string(candidate_usr.join(".stateID")).unwrap(),
                fixture.candidate.id.to_string()
            );
            assert_generated_snapshot(
                &system_model::snapshot_path(&installation.root),
                &fixture.previous_snapshot,
                "previous-package",
            );
            assert!(
                !system_model::snapshot_path(&installation.staging_dir()).exists(),
                "root ABI preflight decorated the untouched candidate"
            );
            assert!(!live_usr.join(".cast-tree-id").exists());
            assert!(!candidate_usr.join(".cast-tree-id").exists());
            assert_eq!(fixture.client.state_db.all().unwrap(), states);
            assert!(!installation.root_path(fixture.candidate.id.to_string()).exists());
            assert!(
                fs::read_dir(installation.state_quarantine_dir())
                    .unwrap()
                    .next()
                    .is_none(),
                "static root ABI conflict quarantined an untouched candidate"
            );
        }
    }
}

#[test]
fn retained_live_root_abi_rejects_replacement_at_the_exchange_boundary() {
    let fixture = stateful_transition_fixture(false);
    let installation = &fixture.client.installation;
    let live_usr = installation.root.join("usr");
    let live_usr_identity = root_abi_inode(&live_usr);
    create_root_links(&installation.root).unwrap();
    let foreign = installation.root.join("bin");
    let mut foreign_identity = None;
    assert!(take_observed_trigger_scopes().is_empty());

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::BeforeUsrExchange {
                    fs::remove_file(&foreign).unwrap();
                    fs::write(&foreign, b"foreign exchange-boundary replacement").unwrap();
                    foreign_identity = Some(root_abi_inode(&foreign));
                }
                Ok(())
            },
        )
        .unwrap_err();

    assert!(matches!(error, Error::StatefulCandidatePreserved { .. }), "{error:#?}");
    assert_eq!(take_observed_trigger_scopes(), ["transaction"]);
    assert_eq!(root_abi_inode(&foreign), foreign_identity.unwrap());
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign exchange-boundary replacement");
    assert_eq!(root_abi_inode(&live_usr), live_usr_identity);
    assert_eq!(
        fs::read_to_string(live_usr.join(".stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_fresh_candidate_quarantined_and_invalidated(&fixture);
}

#[test]
fn retained_absent_root_abi_rejects_appearance_at_the_exchange_boundary() {
    let fixture = stateful_transition_fixture(false);
    let installation = &fixture.client.installation;
    let live_usr = installation.root.join("usr");
    let live_usr_identity = root_abi_inode(&live_usr);
    let foreign = installation.root.join("bin");
    let mut foreign_identity = None;
    assert_root_abi_absent(&foreign);
    assert!(take_observed_trigger_scopes().is_empty());

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::BeforeUsrExchange {
                    fs::write(&foreign, b"foreign entry after retained absence").unwrap();
                    foreign_identity = Some(root_abi_inode(&foreign));
                }
                Ok(())
            },
        )
        .unwrap_err();

    assert!(matches!(error, Error::StatefulCandidatePreserved { .. }), "{error:#?}");
    assert_eq!(take_observed_trigger_scopes(), ["transaction"]);
    assert_eq!(root_abi_inode(&foreign), foreign_identity.unwrap());
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign entry after retained absence");
    assert_eq!(root_abi_inode(&live_usr), live_usr_identity);
    assert_eq!(
        fs::read_to_string(live_usr.join(".stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_fresh_candidate_quarantined_and_invalidated(&fixture);
}

#[test]
fn post_exchange_root_abi_publication_conflict_reverses_usr_and_preserves_foreign_entry() {
    let fixture = stateful_transition_fixture(false);
    let installation = &fixture.client.installation;
    let live_usr = installation.root.join("usr");
    let foreign = installation.root.join("bin");
    let publication_saw_candidate = Rc::new(Cell::new(false));
    let foreign_identity = Rc::new(Cell::new(None));
    let hook_saw_candidate = Rc::clone(&publication_saw_candidate);
    let hook_foreign_identity = Rc::clone(&foreign_identity);
    let hook_live_usr = live_usr.clone();
    let hook_foreign = foreign.clone();
    let candidate = fixture.candidate.id;
    assert_root_abi_absent(&foreign);
    assert!(take_observed_trigger_scopes().is_empty());

    arm_before_stateful_root_abi_publication(move || {
        hook_saw_candidate.set(fs::read_to_string(hook_live_usr.join(".stateID")).unwrap() == candidate.to_string());
        fs::write(&hook_foreign, b"foreign post-exchange publication winner").unwrap();
        hook_foreign_identity.set(Some(root_abi_inode(&hook_foreign)));
    });

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |_| Ok(()),
        )
        .unwrap_err();

    assert!(
        publication_saw_candidate.get(),
        "root ABI publication ran before /usr exchange"
    );
    assert!(matches!(
        error,
        Error::StatefulTransitionUsrRestored {
            candidate: failed,
            previous: Some(previous),
            primary,
        } if failed == fixture.candidate.id
            && previous == fixture.previous.id
            && matches!(
                primary.as_ref(),
                Error::RootAbiLinkTypeConflict { path, .. } if path == &foreign
            )
    ));
    assert_eq!(take_observed_trigger_scopes(), ["transaction"]);
    assert_eq!(root_abi_inode(&foreign), foreign_identity.get().unwrap());
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign post-exchange publication winner");
    assert_fresh_candidate_quarantined_and_invalidated(&fixture);
}
