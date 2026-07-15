use super::{
    ArchiveBaseline, ArchivedStateRepairError, ArchivedStateRepairIdentity, STAGING_NAME, error::namespace_observation,
    fault_injection::between_layout_reads,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RepairLayout {
    Initial,
    CandidateCanonical,
    Complete,
    Preserved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NamedWrapper {
    Absent,
    Candidate,
    Previous,
    Replacement,
    Foreign,
}

impl NamedWrapper {
    fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Candidate => "candidate",
            Self::Previous => "previous",
            Self::Replacement => "replacement",
            Self::Foreign => "foreign",
        }
    }
}

/// Exact evidence returned by one name reopen. Roles alone are insufficient:
/// replacing one foreign directory with another between passes must not look
/// like a stable `Foreign` observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NamedIdentity {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
}

impl NamedIdentity {
    fn of(directory: &super::super::RetainedDirectory) -> Self {
        Self {
            device: directory.witness.device,
            inode: directory.witness.inode,
            owner: directory.witness.owner,
            mode: directory.witness.mode,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NamespaceSnapshot {
    canonical: Option<NamedIdentity>,
    staging: Option<NamedIdentity>,
    quarantine: Option<NamedIdentity>,
}

impl ArchivedStateRepairIdentity {
    /// Classify only a stable, exact C/S/Q namespace observation.
    ///
    /// The first pass opens canonical, staging, then quarantine. The second
    /// pass reopens the same names in reverse order. Exact identities (and
    /// exact absence) must match across both passes. This bounded sandwich
    /// prevents one sequential sample from combining two rename layouts.
    pub(super) fn layout(&self) -> Result<RepairLayout, ArchivedStateRepairError> {
        let before = self.read_layout_forward()?;
        between_layout_reads();
        let after = self.read_layout_reverse()?;
        if before != after {
            return Err(ArchivedStateRepairError::NamespaceChangedDuringObservation);
        }
        self.classify_snapshot(before)
    }

    fn read_layout_forward(&self) -> Result<NamespaceSnapshot, ArchivedStateRepairError> {
        Ok(NamespaceSnapshot {
            canonical: self.open_canonical_identity()?,
            staging: self.open_staging_identity()?,
            quarantine: self.open_quarantine_identity()?,
        })
    }

    fn read_layout_reverse(&self) -> Result<NamespaceSnapshot, ArchivedStateRepairError> {
        let quarantine = self.open_quarantine_identity()?;
        let staging = self.open_staging_identity()?;
        let canonical = self.open_canonical_identity()?;
        Ok(NamespaceSnapshot {
            canonical,
            staging,
            quarantine,
        })
    }

    fn open_canonical_identity(&self) -> Result<Option<NamedIdentity>, ArchivedStateRepairError> {
        let path = self.roots.path.join(self.state_name.to_string_lossy().as_ref());
        self.roots
            .open_optional_child(&self.state_name, path)
            .map(|named| named.as_ref().map(NamedIdentity::of))
            .map_err(|source| namespace_observation("open canonical archived-state wrapper", source))
    }

    fn open_staging_identity(&self) -> Result<Option<NamedIdentity>, ArchivedStateRepairError> {
        self.roots
            .open_optional_child(STAGING_NAME, self.staging.path.clone())
            .map(|named| named.as_ref().map(NamedIdentity::of))
            .map_err(|source| namespace_observation("open fixed archived-repair staging wrapper", source))
    }

    fn open_quarantine_identity(&self) -> Result<Option<NamedIdentity>, ArchivedStateRepairError> {
        self.quarantine
            .open_optional_child(&self.quarantine_name, self.quarantine_path.clone())
            .map(|named| named.as_ref().map(NamedIdentity::of))
            .map_err(|source| namespace_observation("open archived-repair quarantine name", source))
    }

    fn classify_snapshot(&self, snapshot: NamespaceSnapshot) -> Result<RepairLayout, ArchivedStateRepairError> {
        let role = |named: Option<NamedIdentity>| {
            let Some(named) = named else {
                return NamedWrapper::Absent;
            };
            if named == NamedIdentity::of(&self.staging) {
                return NamedWrapper::Candidate;
            }
            if named == NamedIdentity::of(&self.replacement) {
                return NamedWrapper::Replacement;
            }
            if let ArchiveBaseline::Existing(previous) = &self.archive
                && named == NamedIdentity::of(previous)
            {
                return NamedWrapper::Previous;
            }
            NamedWrapper::Foreign
        };

        let canonical = role(snapshot.canonical);
        let staged = role(snapshot.staging);
        let quarantined = role(snapshot.quarantine);
        let layout = match (&self.archive, canonical, staged, quarantined) {
            (
                ArchiveBaseline::Existing(_),
                NamedWrapper::Previous,
                NamedWrapper::Candidate,
                NamedWrapper::Replacement,
            )
            | (ArchiveBaseline::Missing, NamedWrapper::Absent, NamedWrapper::Candidate, NamedWrapper::Replacement) => {
                RepairLayout::Initial
            }
            (
                ArchiveBaseline::Existing(_),
                NamedWrapper::Candidate,
                NamedWrapper::Previous,
                NamedWrapper::Replacement,
            )
            | (ArchiveBaseline::Missing, NamedWrapper::Candidate, NamedWrapper::Absent, NamedWrapper::Replacement) => {
                RepairLayout::CandidateCanonical
            }
            (
                ArchiveBaseline::Existing(_),
                NamedWrapper::Candidate,
                NamedWrapper::Replacement,
                NamedWrapper::Previous,
            )
            | (ArchiveBaseline::Missing, NamedWrapper::Candidate, NamedWrapper::Replacement, NamedWrapper::Absent) => {
                RepairLayout::Complete
            }
            (
                ArchiveBaseline::Existing(_),
                NamedWrapper::Previous,
                NamedWrapper::Replacement,
                NamedWrapper::Candidate,
            )
            | (ArchiveBaseline::Missing, NamedWrapper::Absent, NamedWrapper::Replacement, NamedWrapper::Candidate) => {
                RepairLayout::Preserved
            }
            _ => {
                return Err(ArchivedStateRepairError::NamespaceMismatch {
                    canonical: canonical.as_str(),
                    staging: staged.as_str(),
                    quarantine: quarantined.as_str(),
                });
            }
        };
        Ok(layout)
    }

    pub(super) fn require_initial_layout(&self) -> Result<(), ArchivedStateRepairError> {
        self.require_layout(RepairLayout::Initial)
    }

    pub(super) fn require_layout(&self, expected: RepairLayout) -> Result<(), ArchivedStateRepairError> {
        let actual = self.layout()?;
        if actual == expected {
            Ok(())
        } else {
            Err(layout_mismatch(expected, actual))
        }
    }
}

fn layout_mismatch(expected: RepairLayout, actual: RepairLayout) -> ArchivedStateRepairError {
    ArchivedStateRepairError::NamespaceMismatch {
        canonical: layout_name(actual),
        staging: "layout",
        quarantine: layout_name(expected),
    }
}

fn layout_name(layout: RepairLayout) -> &'static str {
    match layout {
        RepairLayout::Initial => "initial",
        RepairLayout::CandidateCanonical => "candidate-canonical",
        RepairLayout::Complete => "complete",
        RepairLayout::Preserved => "preserved",
    }
}
