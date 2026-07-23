use super::*;

impl StatefulTreeIdentity {
    /// Publish a failed candidate into one deterministic, token-derived
    /// quarantine slot and make the rename durable before its database
    /// correlation may be removed.
    pub(crate) fn quarantine_candidate(
        &self,
        installation: &Installation,
        candidate: state::Id,
        kind: FailedCandidateKind,
    ) -> Result<QuarantinedCandidate, Error> {
        self.require_no_journal()?;
        installation.revalidate_root_directory()?;

        let staging_path = installation.staging_dir();
        let source_path = installation.staging_path("usr");
        let quarantine_path = installation.state_quarantine_dir();
        let staging =
            RetainedDirectory::open_beneath(installation.root_directory(), STAGING_RELATIVE, staging_path.clone())?;
        let quarantine = RetainedDirectory::open_beneath(
            installation.root_directory(),
            QUARANTINE_RELATIVE,
            quarantine_path.clone(),
        )?;
        if staging.witness.device != quarantine.witness.device {
            return Err(Error::QuarantineCrossDevice {
                source_path: staging_path,
                destination: quarantine_path,
            });
        }

        let name = QuarantineName::parse(format!(
            "failed-{}-{candidate}-{}",
            kind.as_str(),
            self.candidate.marker.token().as_str()
        ))
        .map_err(Error::InvalidQuarantineName)?;
        let encoded_name = std::ffi::CString::new(name.as_str())
            .map_err(|source| quarantine_io("encode quarantine slot name", &quarantine.path, source.into()))?;
        let slot_path = quarantine.path.join(name.as_str());
        let destination_path = slot_path.join("usr");
        let mut retained_attempt = self
            .quarantine_attempt
            .lock()
            .map_err(|_| Error::QuarantineAttemptLockPoisoned)?;
        let existing_slot = quarantine.open_optional_child(&encoded_name, slot_path.clone())?;
        let (slot, already_moved) = match (retained_attempt.as_ref(), existing_slot) {
            (None, None) => {
                self.candidate.verify_named_read_only(&source_path)?;
                quarantine_checkpoint(QuarantineFaultPoint::CandidatePreSync)?;
                self.sync_candidate_for_recovery(&source_path)?;
                let slot = RetainedDirectory::create_private_child(&quarantine, &encoded_name, slot_path.clone())?;
                *retained_attempt = Some(RetainedQuarantineAttempt {
                    name: encoded_name.clone(),
                    slot: slot.clone_retained()?,
                });
                quarantine_checkpoint(QuarantineFaultPoint::SlotSync)?;
                slot.sync("sync empty failed-candidate quarantine slot")?;
                quarantine_checkpoint(QuarantineFaultPoint::QuarantineBaseSync)?;
                quarantine.sync("sync quarantine base after slot creation")?;
                (slot, false)
            }
            (None, Some(_)) => {
                return Err(Error::QuarantineSlotExists { path: slot_path });
            }
            (Some(_), None) => {
                return Err(Error::QuarantineDirectoryChanged { path: slot_path });
            }
            (Some(attempt), Some(slot)) => {
                if attempt.name != encoded_name {
                    return Err(Error::QuarantineAttemptChanged {
                        expected: attempt.name.to_string_lossy().into_owned(),
                        actual: name.as_str().to_owned(),
                    });
                }
                attempt.slot.require_same(&slot)?;
                if slot.witness.mode != PRIVATE_DIRECTORY_MODE {
                    return Err(Error::UnsafeQuarantineDirectory {
                        path: slot.path.clone(),
                        owner: slot.witness.owner,
                        mode: slot.witness.mode,
                    });
                }
                match slot.entries(2)?.as_slice() {
                    [] => {
                        self.candidate.verify_named_read_only(&source_path)?;
                        quarantine_checkpoint(QuarantineFaultPoint::CandidatePreSync)?;
                        self.sync_candidate_for_recovery(&source_path)?;
                        quarantine_checkpoint(QuarantineFaultPoint::SlotSync)?;
                        slot.sync("resync empty failed-candidate quarantine slot")?;
                        quarantine_checkpoint(QuarantineFaultPoint::QuarantineBaseSync)?;
                        quarantine.sync("resync quarantine base for resumed publication")?;
                        (slot, false)
                    }
                    [entry] if entry.as_slice() == b"usr" => {
                        staging.require_child_absent(LIVE_USR_NAME)?;
                        let moved = slot.open_child(LIVE_USR_NAME, destination_path.clone())?;
                        self.candidate
                            .verify_store_read_only(&TreeMarkerStore::open(&moved.file, &destination_path)?)?;
                        self.candidate.verify_named_read_only(&destination_path)?;
                        (slot, true)
                    }
                    entries => {
                        return Err(Error::UnexpectedQuarantineEntries {
                            path: slot.path.clone(),
                            entries: entries
                                .iter()
                                .map(|name| String::from_utf8_lossy(name).into_owned())
                                .collect(),
                        });
                    }
                }
            }
        };
        drop(retained_attempt);

        if !already_moved {
            staging.revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)?;
            quarantine.revalidate_beneath(installation.root_directory(), QUARANTINE_RELATIVE)?;
            slot.revalidate_child(&quarantine, &encoded_name)?;
            slot.require_child_absent(LIVE_USR_NAME)?;
            slot.require_exact_entries(&[])?;
            let source_usr = staging.open_child(LIVE_USR_NAME, source_path.clone())?;
            self.candidate
                .verify_store_read_only(&TreeMarkerStore::open(&source_usr.file, &source_path)?)?;
            self.candidate.verify_named_read_only(&source_path)?;

            quarantine_checkpoint(QuarantineFaultPoint::Rename)?;
            renameat2_noreplace(&staging.file, LIVE_USR_NAME, &slot.file, LIVE_USR_NAME)
                .map_err(|source| quarantine_io("move failed candidate into quarantine", &slot_path, source))?;
        }

        staging.require_child_absent(LIVE_USR_NAME)?;
        slot.require_exact_entries(&[b"usr"])?;
        let moved = slot.open_child(LIVE_USR_NAME, destination_path.clone())?;
        self.candidate
            .verify_store_read_only(&TreeMarkerStore::open(&moved.file, &destination_path)?)?;
        self.candidate.verify_named_read_only(&destination_path)?;

        quarantine_checkpoint(QuarantineFaultPoint::MovedCandidateSync)?;
        self.sync_candidate_for_recovery(&destination_path)?;
        quarantine_checkpoint(QuarantineFaultPoint::SourceParentSync)?;
        staging.sync("sync staging after failed-candidate removal")?;
        quarantine_checkpoint(QuarantineFaultPoint::DestinationParentSync)?;
        slot.sync("sync quarantine slot after failed-candidate publication")?;
        quarantine.sync("resync quarantine base after failed-candidate publication")?;

        let quarantined = QuarantinedCandidate {
            name: encoded_name,
            destination_path,
            staging,
            quarantine,
            slot,
        };
        quarantine_checkpoint(QuarantineFaultPoint::FinalRevalidation)?;
        self.revalidate_quarantined_candidate(installation, &quarantined)?;

        Ok(quarantined)
    }

    /// Repeat the complete durability and identity proof immediately before a
    /// fresh candidate's database correlation is removed.
    pub(crate) fn revalidate_quarantined_candidate(
        &self,
        installation: &Installation,
        quarantined: &QuarantinedCandidate,
    ) -> Result<(), Error> {
        self.require_no_journal()?;
        self.sync_candidate_for_recovery(&quarantined.destination_path)?;
        quarantined
            .staging
            .sync("resync staging before candidate invalidation")?;
        quarantined
            .slot
            .sync("resync quarantine slot before candidate invalidation")?;
        quarantined
            .quarantine
            .sync("resync quarantine base before candidate invalidation")?;
        installation.revalidate_root_directory()?;
        quarantined
            .staging
            .revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)?;
        quarantined
            .quarantine
            .revalidate_beneath(installation.root_directory(), QUARANTINE_RELATIVE)?;
        quarantined
            .slot
            .revalidate_child(&quarantined.quarantine, &quarantined.name)?;
        quarantined.staging.require_child_absent(LIVE_USR_NAME)?;
        quarantined.slot.require_exact_entries(&[b"usr"])?;
        let moved = quarantined
            .slot
            .open_child(LIVE_USR_NAME, quarantined.destination_path.clone())?;
        self.candidate
            .verify_store_read_only(&TreeMarkerStore::open(&moved.file, &quarantined.destination_path)?)?;
        self.candidate.verify_named_read_only(&quarantined.destination_path)
    }
}
