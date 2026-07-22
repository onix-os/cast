use super::*;
use std::os::fd::AsRawFd as _;

#[test]
fn all_already_exact_terminal_evidence_promotes_without_republishing() {
    with_staged_alias_attempt!(
        before_stage |_client, plan, _inventory, _claims, _predecessor, _deadline, topology_fixture| {
            for output in plan.outputs() {
                let bytes = match output.generated_bytes() {
                    Some(bytes) => bytes.to_vec(),
                    None => {
                        let asset = output
                            .sealed_asset()
                            .unwrap()
                            .expect("sealed publication output retains its exact source");
                        let mut bytes = vec![0_u8; output.expected_length() as usize];
                        let mut offset = 0usize;
                        while offset < bytes.len() {
                            let read = unsafe {
                                nix::libc::pread(
                                    asset.descriptor().as_raw_fd(),
                                    bytes[offset..].as_mut_ptr().cast(),
                                    bytes.len() - offset,
                                    offset as nix::libc::off_t,
                                )
                            };
                            assert!(read > 0, "read exact sealed publication source");
                            offset += read as usize;
                        }
                        bytes
                    }
                };
                let destination = topology_fixture
                    .publication_root()
                    .join(output.relative_path());
                fs::create_dir_all(destination.parent().unwrap()).unwrap();
                set_safe_publication_parents(
                    topology_fixture.publication_root(),
                    output.relative_path(),
                );
                fs::write(&destination, bytes).unwrap();
                fs::set_permissions(
                    &destination,
                    fs::Permissions::from_mode(output.mode()),
                )
                .unwrap();
            }
        },
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {

            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            assert_eq!(terminal.published_count(), 0);
            assert_eq!(terminal.already_exact_count(), plan.publication_count());
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 4);
            let promoted = terminal.promote_terminal_receipt(&client).unwrap();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert_eq!(
                promoted.database_outcome(),
                BootPublicationReceiptPromotionOutcome::Promoted,
            );
            assert_eq!(promoted.published_count(), 0);
            assert_eq!(
                promoted.already_exact_count(),
                plan.publication_count(),
            );
            assert_promoted_state(
                &fixture.state_db.boot_publication_receipt_state().unwrap(),
                fingerprint,
            );
        }
    );
}

#[test]
fn chained_predecessor_terminal_receipt_promotes_and_retains_the_chain() {
    with_staged_alias_attempt!(
        before_stage |client, plan, inventory, claims, journal_predecessor, deadline| {
            let mut prior_record = journal_predecessor.clone();
            prior_record.transition_id =
                TransitionId::parse("fedcba9876543210fedcba9876543210").unwrap();
            let prior_receipt = plan
                .prepare_complete_boot_publication_receipt(
                    inventory,
                    &prior_record,
                    None,
                    claims,
                )
                .unwrap();
            client
                .state_db
                .stage_boot_publication_receipt(&prior_receipt)
                .unwrap();
            assert_eq!(
                client
                    .state_db
                    .promote_boot_publication_receipt(&prior_receipt, deadline)
                    .unwrap(),
                BootPublicationReceiptPromotionOutcome::Promoted,
            );
        },
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            assert!(staged.receipt().body().committed_predecessor().is_some());
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 4);
            let promoted = terminal.promote_terminal_receipt(&client).unwrap();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert_eq!(
                promoted.database_outcome(),
                BootPublicationReceiptPromotionOutcome::Promoted,
            );
            let state = fixture.state_db.boot_publication_receipt_state().unwrap();
            assert_promoted_state(&state, fingerprint);
            assert!(
                state
                    .committed()
                    .unwrap()
                    .body()
                    .committed_predecessor()
                    .is_some(),
            );
        }
    );
}

#[test]
fn mixed_terminal_evidence_promotes_once_and_preserves_journal_and_counters() {
    with_staged_alias_attempt!(
        before_stage |_client, plan, _inventory, _claims, _predecessor, _deadline, topology_fixture| {
            let exact = plan
                .outputs()
                .find_map(|output| {
                    output.generated_bytes().map(|bytes| {
                        (output.relative_path().to_owned(), bytes.to_vec())
                    })
                })
                .expect("fixture plan contains generated output");
            let exact_path = topology_fixture.publication_root().join(&exact.0);
            fs::create_dir_all(exact_path.parent().unwrap()).unwrap();
            set_safe_publication_parents(topology_fixture.publication_root(), &exact.0);
            fs::write(&exact_path, &exact.1).unwrap();
            fs::set_permissions(&exact_path, fs::Permissions::from_mode(0o644)).unwrap();
        },
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, fingerprint| {

            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            assert_eq!(terminal.already_exact_count(), 1);
            let evidence = evidence_snapshot(terminal.evidence());
            let record_path = fixture
                .installation
                .root
                .join(".cast/journal/state-transition");
            let record_inode = fs::metadata(&record_path).unwrap().ino();
            let record_bytes = fs::read(&record_path).unwrap();

            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 4);
            let promoted = terminal.promote_terminal_receipt(&client).unwrap();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);

            assert_eq!(
                promoted.database_outcome(),
                BootPublicationReceiptPromotionOutcome::Promoted,
            );
            assert_eq!(promoted.receipt_fingerprint(), fingerprint);
            assert_eq!(promoted.publication_count(), plan.publication_count());
            assert_eq!(promoted.published_count(), plan.publication_count() - 1);
            assert_eq!(promoted.already_exact_count(), 1);
            assert_eq!(evidence_snapshot(promoted.evidence()), evidence);
            assert_eq!(fs::metadata(&record_path).unwrap().ino(), record_inode);
            assert_eq!(fs::read(&record_path).unwrap(), record_bytes);
            assert_promoted_state(
                &fixture.state_db.boot_publication_receipt_state().unwrap(),
                fingerprint,
            );

            drop(promoted);
            let cast = fixture
                .installation
                .retained_mutable_cast_directory()
                .unwrap();
            let journal = TransitionJournalStore::open_in_retained_cast(
                cast,
                &fixture.installation.root,
            )
            .unwrap();
            assert_eq!(
                journal.load_revalidated_retained_cast(cast).unwrap(),
                Some(expected_record),
            );
        }
    );
}

#[test]
fn exact_already_promoted_receipt_is_adopted_without_journal_change() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            assert_eq!(
                fixture
                    .state_db
                    .promote_boot_publication_receipt(
                        terminal.staged.receipt(),
                        plan.input_deadline(),
                    )
                    .unwrap(),
                BootPublicationReceiptPromotionOutcome::Promoted,
            );
            let record_path = fixture
                .installation
                .root
                .join(".cast/journal/state-transition");
            let record_inode = fs::metadata(&record_path).unwrap().ino();
            let record_bytes = fs::read(&record_path).unwrap();

            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 4);
            let promoted = terminal.promote_terminal_receipt(&client).unwrap();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert_eq!(
                promoted.database_outcome(),
                BootPublicationReceiptPromotionOutcome::AlreadyPromoted,
            );
            assert_eq!(fs::metadata(&record_path).unwrap().ino(), record_inode);
            assert_eq!(fs::read(&record_path).unwrap(), record_bytes);
            assert_eq!(expected_record.phase, Phase::BootSyncStarted);
            assert_promoted_state(
                &fixture.state_db.boot_publication_receipt_state().unwrap(),
                fingerprint,
            );
        }
    );
}
