//! Authenticated local command-line policy for one ActiveReblit boot repair.
//!
//! The legacy publisher discovers `/etc/kernel/cmdline.d` through mutable
//! pathnames and silently ignores several failures.  This module instead
//! captures either an exact retained absence proof or a bounded, sorted set of
//! regular snippets and `/dev/null` masks below the authenticated installation
//! root. Revalidation performs two complete content passes, retraverses and
//! rebinds every pathname component, and sandwiches them with bounded public
//! installation-root checks. It performs no rendering and grants no
//! publication authority.
//!
//! Canonical directories and regular files reject every xattr, including
//! security labels; labeled deployments must provide an explicit future
//! policy rather than being silently accepted. Linux does not expose a
//! generally usable descriptor-only symlink-xattr audit for this baseline, so
//! masks claim only same-owner/single-link identity plus an exact retained-fd
//! `/dev/null` target and length. Symlink mode, ACL, and xattr absence are not
//! claimed.

use std::{
    ffi::OsStr,
    marker::PhantomData,
    os::unix::ffi::OsStrExt as _,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use thiserror::Error;

use crate::{Installation, installation};

use self::filesystem::{
    RetainedLocalPolicyLocation, capture_entry, capture_location, inventory_names, revalidate_entry,
    revalidate_location,
};

#[path = "active_reblit_local_boot_policy/filesystem.rs"]
mod filesystem;

const KIB: usize = 1024;
const MAX_LOCAL_POLICY_DIRECTORY_ENTRIES: usize = 256;
const MAX_LOCAL_CMDLINE_ENTRIES: usize = 128;
const MAX_LOCAL_POLICY_NAME_BYTES: usize = 255;
const MAX_LOCAL_POLICY_TOTAL_NAME_BYTES: usize = 64 * KIB;
const MAX_LOCAL_CMDLINE_FILE_BYTES: usize = 64 * KIB;
const MAX_LOCAL_CMDLINE_TOTAL_BYTES: usize = 256 * KIB;
const MAX_LOCAL_POLICY_WORK: usize = 16_384;
const LOCAL_POLICY_TIMEOUT: Duration = Duration::from_secs(30);

const LOCAL_BOOT_POLICY: LocalBootPolicy = LocalBootPolicy {
    max_directory_entries: MAX_LOCAL_POLICY_DIRECTORY_ENTRIES,
    max_cmdline_entries: MAX_LOCAL_CMDLINE_ENTRIES,
    max_name_bytes: MAX_LOCAL_POLICY_NAME_BYTES,
    max_total_name_bytes: MAX_LOCAL_POLICY_TOTAL_NAME_BYTES,
    max_file_bytes: MAX_LOCAL_CMDLINE_FILE_BYTES,
    max_total_file_bytes: MAX_LOCAL_CMDLINE_TOTAL_BYTES,
    max_work: MAX_LOCAL_POLICY_WORK,
    timeout: LOCAL_POLICY_TIMEOUT,
};

/// One non-cloneable local-policy snapshot prepared before any boot effect.
///
/// The retained location and entry descriptors remain private.  Consumers get
/// borrowed semantic views only after an exact same-thread revalidation.
pub(in crate::client) struct PreparedActiveReblitLocalBootPolicy {
    location: RetainedLocalPolicyLocation,
    inventory: Vec<Box<[u8]>>,
    entries: Vec<RetainedLocalCmdlineEntry>,
    total_file_bytes: usize,
    #[cfg(test)]
    preparation_work: usize,
}

/// Lifetime-bound views into a freshly revalidated policy snapshot.
pub(in crate::client) struct RevalidatedActiveReblitLocalBootPolicy<'a> {
    policy: &'a PreparedActiveReblitLocalBootPolicy,
    _installation: &'a Installation,
    _same_thread: PhantomData<Rc<()>>,
}

/// One deterministic local policy action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum BoundActiveReblitLocalCmdlineEntry<'a> {
    /// Append the normalized snippet after package-owned command-line data.
    Append { name: &'a OsStr, snippet: &'a str },
    /// Suppress the package-owned snippet having this exact filename.
    Mask { name: &'a OsStr },
}

enum RetainedLocalCmdlineEntry {
    Append {
        name: Box<[u8]>,
        retained: std::fs::File,
        witness: filesystem::EntryWitness,
        raw: Box<[u8]>,
        snippet: Box<str>,
    },
    Mask {
        name: Box<[u8]>,
        retained: std::fs::File,
        witness: filesystem::EntryWitness,
    },
}

impl RetainedLocalCmdlineEntry {
    fn name_bytes(&self) -> &[u8] {
        match self {
            Self::Append { name, .. } | Self::Mask { name, .. } => name,
        }
    }

    fn bound(&self) -> BoundActiveReblitLocalCmdlineEntry<'_> {
        match self {
            Self::Append { name, snippet, .. } => BoundActiveReblitLocalCmdlineEntry::Append {
                name: OsStr::from_bytes(name),
                snippet,
            },
            Self::Mask { name, .. } => BoundActiveReblitLocalCmdlineEntry::Mask {
                name: OsStr::from_bytes(name),
            },
        }
    }
}

impl PreparedActiveReblitLocalBootPolicy {
    pub(in crate::client) fn prepare(installation: &Installation) -> Result<Self, ActiveReblitLocalBootPolicyError> {
        prepare_with_policy_and_checkpoint(installation, LocalBootPolicy::production(), |_| {})
    }

    /// Revalidate the exact retained absence or directory, both sorted name
    /// inventories, every relevant inode, and every regular-file byte.
    pub(in crate::client) fn revalidate<'a>(
        &'a self,
        installation: &'a Installation,
    ) -> Result<RevalidatedActiveReblitLocalBootPolicy<'a>, ActiveReblitLocalBootPolicyError> {
        let mut budget = LocalBootPolicyBudget::new(LocalBootPolicy::production())?;
        self.revalidate_with_budget(installation, &mut budget)?;
        Ok(RevalidatedActiveReblitLocalBootPolicy {
            policy: self,
            _installation: installation,
            _same_thread: PhantomData,
        })
    }

    fn revalidate_with_budget(
        &self,
        installation: &Installation,
        budget: &mut LocalBootPolicyBudget,
    ) -> Result<(), ActiveReblitLocalBootPolicyError> {
        self.revalidate_with_budget_and_checkpoints(installation, budget, || {}, || {})
    }

    fn revalidate_with_budget_and_checkpoints<F, G>(
        &self,
        installation: &Installation,
        budget: &mut LocalBootPolicyBudget,
        after_first_contents: F,
        between_passes: G,
    ) -> Result<(), ActiveReblitLocalBootPolicyError>
    where
        F: FnOnce(),
        G: FnOnce(),
    {
        revalidate_installation_root(installation, budget)?;
        self.revalidate_complete_pass(installation, budget, after_first_contents)?;
        revalidate_installation_root(installation, budget)?;
        between_passes();
        revalidate_installation_root(installation, budget)?;
        self.revalidate_complete_pass(installation, budget, || {})?;
        revalidate_installation_root(installation, budget)?;
        budget.require_deadline(&local_policy_path(installation))
    }

    fn revalidate_complete_pass<F>(
        &self,
        installation: &Installation,
        budget: &mut LocalBootPolicyBudget,
        before_final_rebind: F,
    ) -> Result<(), ActiveReblitLocalBootPolicyError>
    where
        F: FnOnce(),
    {
        let Some(directory) = revalidate_location(installation, &self.location, budget)? else {
            if !self.inventory.is_empty() || !self.entries.is_empty() || self.total_file_bytes != 0 {
                return Err(ActiveReblitLocalBootPolicyError::Changed {
                    path: local_policy_path(installation),
                    reason: "retained absence unexpectedly owns policy entries",
                });
            }
            before_final_rebind();
            if revalidate_location(installation, &self.location, budget)?.is_some() {
                return Err(ActiveReblitLocalBootPolicyError::Changed {
                    path: local_policy_path(installation),
                    reason: "retained local-policy absence became present before final rebind",
                });
            }
            return Ok(());
        };

        let before = inventory_names(&directory, &local_policy_path(installation), budget)?;
        require_same_inventory(&self.inventory, &before, installation)?;
        for entry in &self.entries {
            revalidate_entry(&directory, entry, installation, budget)?;
        }
        let after = inventory_names(&directory, &local_policy_path(installation), budget)?;
        require_same_inventory(&self.inventory, &after, installation)?;
        filesystem::require_present_directory_witness(
            &directory,
            self.location.present_witness().expect("present location has a witness"),
            &local_policy_path(installation),
            budget,
        )?;
        before_final_rebind();
        let rebound = revalidate_location(installation, &self.location, budget)?.ok_or_else(|| {
            ActiveReblitLocalBootPolicyError::Changed {
                path: local_policy_path(installation),
                reason: "retained local-policy directory became absent before final rebind",
            }
        })?;
        filesystem::require_present_directory_witness(
            &rebound,
            self.location.present_witness().expect("present location has a witness"),
            &local_policy_path(installation),
            budget,
        )
    }

    #[cfg(test)]
    pub(in crate::client) fn is_absent(&self) -> bool {
        self.location.is_absent()
    }

    #[cfg(test)]
    pub(in crate::client) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(in crate::client) fn total_file_bytes(&self) -> usize {
        self.total_file_bytes
    }

    #[cfg(test)]
    fn preparation_work(&self) -> usize {
        self.preparation_work
    }
}

impl RevalidatedActiveReblitLocalBootPolicy<'_> {
    pub(in crate::client) fn is_absent(&self) -> bool {
        self.policy.location.is_absent()
    }

    pub(in crate::client) fn entries(&self) -> impl ExactSizeIterator<Item = BoundActiveReblitLocalCmdlineEntry<'_>> {
        self.policy.entries.iter().map(RetainedLocalCmdlineEntry::bound)
    }

    pub(in crate::client) fn total_file_bytes(&self) -> usize {
        self.policy.total_file_bytes
    }
}

#[derive(Clone, Copy)]
struct LocalBootPolicy {
    max_directory_entries: usize,
    max_cmdline_entries: usize,
    max_name_bytes: usize,
    max_total_name_bytes: usize,
    max_file_bytes: usize,
    max_total_file_bytes: usize,
    max_work: usize,
    timeout: Duration,
}

impl LocalBootPolicy {
    const fn production() -> Self {
        LOCAL_BOOT_POLICY
    }
}

struct LocalBootPolicyBudget {
    policy: LocalBootPolicy,
    deadline: Instant,
    work: usize,
}

impl LocalBootPolicyBudget {
    fn new(policy: LocalBootPolicy) -> Result<Self, ActiveReblitLocalBootPolicyError> {
        let deadline =
            Instant::now()
                .checked_add(policy.timeout)
                .ok_or(ActiveReblitLocalBootPolicyError::InvalidDeadline {
                    timeout: policy.timeout,
                })?;
        Ok(Self {
            policy,
            deadline,
            work: 0,
        })
    }

    fn step(&mut self, path: &Path) -> Result<(), ActiveReblitLocalBootPolicyError> {
        self.require_deadline(path)?;
        let actual = self.work.saturating_add(1);
        if actual > self.policy.max_work {
            return Err(ActiveReblitLocalBootPolicyError::WorkLimit {
                path: path.to_owned(),
                limit: self.policy.max_work,
                actual,
            });
        }
        self.work = actual;
        Ok(())
    }

    fn require_deadline(&self, path: &Path) -> Result<(), ActiveReblitLocalBootPolicyError> {
        if Instant::now() > self.deadline {
            Err(ActiveReblitLocalBootPolicyError::DeadlineExceeded {
                path: path.to_owned(),
                timeout: self.policy.timeout,
            })
        } else {
            Ok(())
        }
    }
}

fn revalidate_installation_root(
    installation: &Installation,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    budget.step(&installation.root)?;
    installation.revalidate_root_directory_until(budget.deadline)?;
    budget.require_deadline(&installation.root)
}

fn prepare_with_policy_and_checkpoint<F>(
    installation: &Installation,
    policy: LocalBootPolicy,
    before_final_revalidation: F,
) -> Result<PreparedActiveReblitLocalBootPolicy, ActiveReblitLocalBootPolicyError>
where
    F: FnOnce(&PreparedActiveReblitLocalBootPolicy),
{
    let mut budget = LocalBootPolicyBudget::new(policy)?;
    revalidate_installation_root(installation, &mut budget)?;
    let location = capture_location(installation, &mut budget)?;
    let (inventory, entries, total_file_bytes) = if let Some(directory) = location.present_directory() {
        let before = inventory_names(directory, &local_policy_path(installation), &mut budget)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(before.len().min(policy.max_cmdline_entries))
            .map_err(|source| ActiveReblitLocalBootPolicyError::Allocation {
                resource: "local command-line entries",
                path: local_policy_path(installation),
                source,
            })?;
        let mut total_file_bytes = 0usize;
        for name in &before {
            if !is_cmdline_name(name) {
                continue;
            }
            validate_cmdline_name(name, installation)?;
            if entries.len() >= policy.max_cmdline_entries {
                return Err(ActiveReblitLocalBootPolicyError::CmdlineEntryLimit {
                    path: local_policy_path(installation),
                    limit: policy.max_cmdline_entries,
                    actual: entries.len().saturating_add(1),
                });
            }
            let entry = capture_entry(directory, name, installation, &mut budget)?;
            if let RetainedLocalCmdlineEntry::Append { raw, .. } = &entry {
                total_file_bytes = total_file_bytes.checked_add(raw.len()).ok_or_else(|| {
                    ActiveReblitLocalBootPolicyError::TotalFileBytesLimit {
                        path: local_policy_path(installation),
                        limit: policy.max_total_file_bytes,
                        actual: usize::MAX,
                    }
                })?;
                if total_file_bytes > policy.max_total_file_bytes {
                    return Err(ActiveReblitLocalBootPolicyError::TotalFileBytesLimit {
                        path: local_policy_path(installation),
                        limit: policy.max_total_file_bytes,
                        actual: total_file_bytes,
                    });
                }
            }
            entries.push(entry);
        }
        let after = inventory_names(directory, &local_policy_path(installation), &mut budget)?;
        require_same_inventory(&before, &after, installation)?;
        (before, entries, total_file_bytes)
    } else {
        (Vec::new(), Vec::new(), 0)
    };

    let prepared = PreparedActiveReblitLocalBootPolicy {
        location,
        inventory,
        entries,
        total_file_bytes,
        #[cfg(test)]
        preparation_work: 0,
    };
    before_final_revalidation(&prepared);
    prepared.revalidate_with_budget(installation, &mut budget)?;
    #[cfg(test)]
    let prepared = PreparedActiveReblitLocalBootPolicy {
        preparation_work: budget.work,
        ..prepared
    };
    Ok(prepared)
}

fn require_same_inventory(
    expected: &[Box<[u8]>],
    actual: &[Box<[u8]>],
    installation: &Installation,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    if expected == actual {
        Ok(())
    } else {
        Err(ActiveReblitLocalBootPolicyError::Changed {
            path: local_policy_path(installation),
            reason: "sorted local-policy directory inventory changed",
        })
    }
}

fn is_cmdline_name(name: &[u8]) -> bool {
    name.len() > b".cmdline".len() && name.ends_with(b".cmdline")
}

fn validate_cmdline_name(name: &[u8], installation: &Installation) -> Result<(), ActiveReblitLocalBootPolicyError> {
    let valid = name.len() <= MAX_LOCAL_POLICY_NAME_BYTES
        && name.is_ascii()
        && name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !matches!(name.first(), Some(b'.' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(ActiveReblitLocalBootPolicyError::InvalidCmdlineName {
            path: local_policy_path(installation).join(OsStr::from_bytes(name)),
        })
    }
}

fn normalize_cmdline(bytes: &[u8], path: &Path) -> Result<Box<str>, ActiveReblitLocalBootPolicyError> {
    if bytes
        .iter()
        .any(|byte| !byte.is_ascii() || (byte.is_ascii_control() && !matches!(byte, b'\n' | b'\r' | b'\t')))
    {
        return Err(ActiveReblitLocalBootPolicyError::InvalidCmdlineContent {
            path: path.to_owned(),
            reason: "content must be printable ASCII with only tab, CR, and LF controls",
        });
    }
    let text = std::str::from_utf8(bytes).expect("ASCII is UTF-8");
    let mut normalized = String::new();
    normalized
        .try_reserve_exact(bytes.len())
        .map_err(|source| ActiveReblitLocalBootPolicyError::Allocation {
            resource: "normalized local command-line bytes",
            path: path.to_owned(),
            source,
        })?;
    let mut first = true;
    for line in text.lines().map(str::trim).filter(|line| !line.starts_with('#')) {
        if !first {
            normalized.push(' ');
        }
        first = false;
        normalized.push_str(line);
    }
    if normalized.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ActiveReblitLocalBootPolicyError::InvalidCmdlineContent {
            path: path.to_owned(),
            reason: "normalized content contains a control-character injection",
        });
    }
    Ok(normalized.into_boxed_str())
}

fn local_policy_path(installation: &Installation) -> PathBuf {
    installation.root.join("etc/kernel/cmdline.d")
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitLocalBootPolicyError {
    #[error("revalidate the authenticated installation root around local boot policy")]
    Installation(#[from] installation::Error),
    #[error("local boot policy deadline {timeout:?} cannot be represented")]
    InvalidDeadline { timeout: Duration },
    #[error("local boot policy exceeded its {timeout:?} deadline at `{}`", path.display())]
    DeadlineExceeded { path: PathBuf, timeout: Duration },
    #[error("local boot policy exceeded its work limit of {limit} at `{}` (actual {actual})", path.display())]
    WorkLimit { path: PathBuf, limit: usize, actual: usize },
    #[error("local boot policy directory exceeded {limit} entries at `{}` (actual {actual})", path.display())]
    DirectoryEntryLimit { path: PathBuf, limit: usize, actual: usize },
    #[error("local boot policy name exceeds {limit} bytes at `{}` (actual {actual})", path.display())]
    NameBytesLimit { path: PathBuf, limit: usize, actual: usize },
    #[error("local boot policy directory exceeded {limit} name bytes at `{}` (actual {actual})", path.display())]
    TotalNameBytesLimit { path: PathBuf, limit: usize, actual: usize },
    #[error("local boot policy contains more than {limit} command-line entries at `{}` (actual {actual})", path.display())]
    CmdlineEntryLimit { path: PathBuf, limit: usize, actual: usize },
    #[error("local command-line file exceeds {limit} bytes at `{}` (actual {actual})", path.display())]
    FileBytesLimit { path: PathBuf, limit: usize, actual: u64 },
    #[error("local command-line files exceed {limit} aggregate bytes at `{}` (actual {actual})", path.display())]
    TotalFileBytesLimit { path: PathBuf, limit: usize, actual: usize },
    #[error("invalid local command-line filename `{}`", path.display())]
    InvalidCmdlineName { path: PathBuf },
    #[error("invalid local command-line content at `{}`: {reason}", path.display())]
    InvalidCmdlineContent { path: PathBuf, reason: &'static str },
    #[error("unsafe local boot policy inode at `{}`: {reason}", path.display())]
    UnsafeInode { path: PathBuf, reason: &'static str },
    #[error("local boot policy changed at `{}`: {reason}", path.display())]
    Changed { path: PathBuf, reason: &'static str },
    #[error("{operation} local boot policy capability `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reserve bounded {resource} for local boot policy at `{}`", path.display())]
    Allocation {
        resource: &'static str,
        path: PathBuf,
        #[source]
        source: std::collections::TryReserveError,
    },
}

#[cfg(test)]
#[path = "active_reblit_local_boot_policy_tests.rs"]
mod tests;
