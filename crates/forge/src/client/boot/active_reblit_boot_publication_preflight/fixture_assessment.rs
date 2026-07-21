//! Test-only real namespace observation for ordinary-directory topology fixtures.

use std::{
    cell::RefCell,
    collections::VecDeque,
    fs::{self, OpenOptions},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::PathBuf,
};

use crate::{
    client::{
        active_reblit_boot_namespace_inputs::BoundActiveReblitBootNamespaceDomain,
        active_reblit_mounted_boot_topology::{
            BootTargetRole, RevalidatedActiveReblitBootPublicationTarget,
        },
    },
    linux_fs::{
        descriptor_boot_namespace::{
            BootNamespaceAssessmentLimits, RetainedBootNamespaceAssessmentLimits,
            assess_retained_boot_namespace_until,
        },
    },
};

use super::{
    BootPublicationNamespaceAssessment, target_identity,
};

pub(in crate::client) enum FixtureBootNamespaceMutation {
    RemoveFile(PathBuf),
    ReplaceFileIdentity { canonical: PathBuf, displaced: PathBuf },
    ReplaceDirectoryIdentity { canonical: PathBuf, displaced: PathBuf },
}

impl FixtureBootNamespaceMutation {
    fn apply(self) {
        match self {
            Self::RemoveFile(path) => fs::remove_file(path).expect("remove fixture publication leaf"),
            Self::ReplaceFileIdentity {
                canonical,
                displaced,
            } => {
                let bytes = fs::read(&canonical).expect("read fixture journal record");
                let mode = fs::metadata(&canonical)
                    .expect("inspect fixture journal record")
                    .permissions()
                    .mode();
                fs::rename(&canonical, &displaced).expect("displace fixture journal record");
                fs::write(&canonical, bytes).expect("replace fixture journal record identity");
                fs::set_permissions(&canonical, fs::Permissions::from_mode(mode))
                    .expect("restore fixture journal mode");
            }
            Self::ReplaceDirectoryIdentity {
                canonical,
                displaced,
            } => {
                let mode = fs::metadata(&canonical)
                    .expect("inspect fixture topology directory")
                    .permissions()
                    .mode();
                fs::rename(&canonical, &displaced).expect("displace fixture topology directory");
                fs::create_dir(&canonical).expect("replace fixture topology directory identity");
                fs::set_permissions(&canonical, fs::Permissions::from_mode(mode))
                    .expect("restore fixture topology-directory mode");
            }
        }
    }
}

pub(in crate::client) struct FixtureBootNamespaceAssessment {
    role: BootTargetRole,
    root: PathBuf,
    before: Option<FixtureBootNamespaceMutation>,
    after: Option<FixtureBootNamespaceMutation>,
}

impl FixtureBootNamespaceAssessment {
    pub(in crate::client) fn new(role: BootTargetRole, root: PathBuf) -> Self {
        Self {
            role,
            root,
            before: None,
            after: None,
        }
    }

    pub(in crate::client) fn before(mut self, mutation: FixtureBootNamespaceMutation) -> Self {
        self.before = Some(mutation);
        self
    }

    pub(in crate::client) fn after(mut self, mutation: FixtureBootNamespaceMutation) -> Self {
        self.after = Some(mutation);
        self
    }
}

thread_local! {
    static ASSESSMENTS: RefCell<VecDeque<FixtureBootNamespaceAssessment>> = const {
        RefCell::new(VecDeque::new())
    };
}

pub(in crate::client) struct FixtureBootNamespaceAssessmentGuard;

impl Drop for FixtureBootNamespaceAssessmentGuard {
    fn drop(&mut self) {
        ASSESSMENTS.with(|assessments| assessments.borrow_mut().clear());
    }
}

pub(in crate::client) fn arm(
    assessments: impl IntoIterator<Item = FixtureBootNamespaceAssessment>,
) -> FixtureBootNamespaceAssessmentGuard {
    let assessments = assessments.into_iter().collect::<VecDeque<_>>();
    assert!(!assessments.is_empty(), "fixture namespace assessment queue is empty");
    ASSESSMENTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_empty(), "fixture namespace assessment queue is already armed");
        *slot = assessments;
    });
    FixtureBootNamespaceAssessmentGuard
}

pub(in crate::client) fn remaining() -> usize {
    ASSESSMENTS.with(|assessments| assessments.borrow().len())
}

pub(super) fn take(
    role: BootTargetRole,
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    domain: &BoundActiveReblitBootNamespaceDomain<'_>,
) -> Option<BootPublicationNamespaceAssessment> {
    let directive = ASSESSMENTS.with(|assessments| assessments.borrow_mut().pop_front())?;
    assert_eq!(directive.role, role, "fixture namespace target role changed");
    if let Some(mutation) = directive.before {
        mutation.apply();
    }
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(
            nix::libc::O_PATH
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW,
        )
        .open(&directive.root)
        .expect("open fixture publication root");
    let metadata = root.metadata().expect("inspect fixture publication root");
    let destination = target.destination();
    assert_eq!(metadata.dev(), destination.raw_device());
    assert_eq!(metadata.ino(), destination.inode());
    let assessment = assess_retained_boot_namespace_until(
        &root,
        domain.requests(),
        domain.expected_sources(),
        BootNamespaceAssessmentLimits::default(),
        RetainedBootNamespaceAssessmentLimits::default(),
        target.deadline(),
    )
    .expect("observe fixture publication namespace");
    let states = assessment.states().to_vec().into_boxed_slice();
    if let Some(mutation) = directive.after {
        mutation.apply();
    }
    Some(BootPublicationNamespaceAssessment::fixture(
        target_identity(target),
        states,
    ))
}
