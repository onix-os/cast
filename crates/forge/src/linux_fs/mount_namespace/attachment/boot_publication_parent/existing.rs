//! No-create retention of an already-existing boot publication parent.
//!
//! Replacement and cleanup paths must never turn a vanished receipt-owned
//! parent into a newly created namespace. The shared retained-parent engine
//! owns descriptor validation and full edge revalidation; this narrow entry
//! fixes its missing-component policy to `ExistingOnly`.

use std::time::Instant;

use super::{
    ParentRetentionMode, RetainedBootPublicationParent,
    RetainedBootPublicationParentError, retain_boot_publication_parent_with,
};
use crate::linux_fs::mount_namespace::RevalidatedTaskRootedAttachment;

impl<'prepared> RevalidatedTaskRootedAttachment<'prepared> {
    /// Retain an exact descendant directory chain only if every component
    /// already exists and remains bound to the authenticated boot root.
    ///
    /// `ENOENT` is terminal even if the component disappeared after an earlier
    /// exact leaf assessment. This entry point never selects the creating or
    /// synchronizing branch of the shared engine.
    pub(crate) fn retain_existing_boot_publication_parent_until<'view>(
        &'view self,
        admitted_parent_components: &[&str],
        deadline: Instant,
    ) -> Result<RetainedBootPublicationParent<'view, 'prepared>, RetainedBootPublicationParentError>
    {
        retain_boot_publication_parent_with(
            self,
            admitted_parent_components,
            ParentRetentionMode::ExistingOnly,
            deadline,
        )
    }
}
