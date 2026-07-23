#[test]
fn journal_coordinator_root_links_complete_authenticates_exact_eexist_at_every_publisher_index() {
    for (index, (name, target)) in ROOT_ABI_PUBLICATION_LINKS.into_iter().enumerate() {
        let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(CandidateKind::Archived, 0);
        let path = fixture.installation.root.join(name);
        let raced_identity = std::rc::Rc::new(std::cell::Cell::new(None));
        let raced_identity_hook = std::rc::Rc::clone(&raced_identity);
        let hook_path = path.clone();
        crate::client::arm_before_retained_root_abi_link_publication(index, move || {
            std::os::unix::fs::symlink(target, &hook_path).unwrap();
            raced_identity_hook.set(Some(root_abi_link_identity(&hook_path)));
        });

        let complete = exchanged.publish_root_abi().unwrap();

        crate::client::assert_before_retained_root_abi_link_publication_consumed();
        assert_eq!(
            root_abi_link_identity(&path),
            raced_identity.get().expect("exact EEXIST callback retained raced inode"),
            "publisher index {index} replaced the authenticated exact winner"
        );
        assert_root_links_complete(&fixture);
        assert_eq!(complete.record().phase, Phase::RootLinksComplete);
        complete.revalidate_retained_authorities().unwrap();
    }
}
#[test]
fn journal_coordinator_root_links_complete_rejects_foreign_eexist_at_every_publisher_index_without_replacement() {
    for (index, (name, _target)) in ROOT_ABI_PUBLICATION_LINKS.into_iter().enumerate() {
        let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(CandidateKind::NewState, 0);
        let source = exchanged.record().clone();
        let database_before = usr_exchange_database_snapshot(&fixture, &source);
        let namespace_before =
            snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root);
        let collision = fixture.installation.root.join(name);
        let collision_hook = collision.clone();
        let raced_identity = std::rc::Rc::new(std::cell::Cell::new(None));
        let raced_identity_hook = std::rc::Rc::clone(&raced_identity);
        crate::client::arm_before_retained_root_abi_link_publication(index, move || {
            std::os::unix::fs::symlink("foreign/root-abi-target", &collision_hook).unwrap();
            raced_identity_hook.set(Some(root_abi_link_identity(&collision_hook)));
        });

        let failure = exchanged.publish_root_abi().unwrap_err();

        crate::client::assert_before_retained_root_abi_link_publication_consumed();
        assert!(matches!(failure, RootAbiPublicationFailure::Publication { .. }));
        assert_usr_exchanged_source(&fixture, &source);
        assert_eq!(fs::read_link(&collision).unwrap(), Path::new("foreign/root-abi-target"));
        assert_eq!(root_abi_link_identity(&collision), raced_identity.get().unwrap());
        assert_eq!(
            snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root),
            namespace_before
        );
        assert_eq!(usr_exchange_database_snapshot(&fixture, &source), database_before);
        for (published_index, (published_name, published_target)) in
            ROOT_ABI_PUBLICATION_LINKS.into_iter().enumerate()
        {
            let path = fixture.installation.root.join(published_name);
            match published_index.cmp(&index) {
                std::cmp::Ordering::Less => {
                    assert_eq!(fs::read_link(path).unwrap(), Path::new(published_target));
                }
                std::cmp::Ordering::Equal => {
                    assert_eq!(fs::read_link(path).unwrap(), Path::new("foreign/root-abi-target"));
                }
                std::cmp::Ordering::Greater => assert_state_metadata_name_absent(&path),
            }
        }
    }
}

#[test]
fn journal_coordinator_root_links_complete_rejects_existing_and_new_exact_target_inode_aba() {
    for (case_name, mask, hook_index, changed_name) in [
        ("preflight-existing", 0b00001, 1, "bin"),
        ("newly-published", 0, 1, "sbin"),
    ] {
        let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(CandidateKind::Archived, mask);
        let source = exchanged.record().clone();
        let path = fixture.installation.root.join(changed_name);
        let displaced = fixture
            .installation
            .root
            .join(format!("{changed_name}.{case_name}.displaced"));
        let hook_path = path.clone();
        let hook_displaced = displaced.clone();
        let identities = std::rc::Rc::new(std::cell::Cell::new(None));
        let identities_hook = std::rc::Rc::clone(&identities);
        crate::client::arm_before_retained_root_abi_link_publication(hook_index, move || {
            identities_hook.set(Some(replace_symlink_with_same_target(
                &hook_path,
                &hook_displaced,
            )));
        });

        let failure = exchanged.publish_root_abi().unwrap_err();

        crate::client::assert_before_retained_root_abi_link_publication_consumed();
        assert!(matches!(failure, RootAbiPublicationFailure::Publication { .. }));
        assert_usr_exchanged_source(&fixture, &source);
        let (original, replacement) = identities.get().expect("ABA callback recorded both inodes");
        assert_eq!(root_abi_link_identity(&displaced), original);
        assert_eq!(root_abi_link_identity(&path), replacement);
        assert_eq!(fs::read_link(&displaced).unwrap(), fs::read_link(&path).unwrap());
        assert_root_links_complete(&fixture);
    }
}
