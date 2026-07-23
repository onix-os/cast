use std::{ffi::OsStr, fmt::Write as _};

use super::{
    ActiveReblitBootRenderInputsError, ActiveReblitCmdlineSource, ActiveReblitCmdlineTokenReason,
    BootRenderInputPolicy, CmdlineTokenRange, PreparedActiveReblitPackageCmdlineInputs,
    RevalidatedActiveReblitLocalBootPolicy, allocation, require_deadline, state,
};
use crate::client::{
    active_reblit_local_boot_policy::BoundActiveReblitLocalCmdlineEntry,
    active_reblit_package_cmdline_inputs::{BoundActiveReblitPackageCmdline, BoundActiveReblitPackageCmdlineScope},
};

pub(super) struct AuditedCmdlineInputs<'a> {
    packages: Vec<AuditedPackageCmdline<'a>>,
    masks: Vec<&'a OsStr>,
    local_appends: Vec<AuditedLocalAppend<'a>>,
}

struct AuditedPackageCmdline<'a> {
    entry: BoundActiveReblitPackageCmdline<'a>,
}

struct AuditedLocalAppend<'a> {
    snippet: &'a str,
}

#[derive(Clone, Copy)]
enum SelectedToken {
    Package { index: u16, start: u32, end: u32 },
    Local { index: u16, start: u32, end: u32 },
    Root,
    Cast,
}

#[derive(Clone, Copy)]
enum SnippetOwner {
    Package(u16),
    Local(u16),
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CmdlineMaterializationCheckpoint {
    Admitted { bytes: usize, tokens: usize },
}

pub(super) struct MaterializedKernelCmdline {
    pub(super) cmdline: Box<str>,
    pub(super) tokens: Box<[CmdlineTokenRange]>,
}

impl<'a> AuditedCmdlineInputs<'a> {
    pub(super) fn prepare(
        packages: &'a PreparedActiveReblitPackageCmdlineInputs<'_>,
        local_policy: &'a RevalidatedActiveReblitLocalBootPolicy<'_>,
        deadline: std::time::Instant,
    ) -> Result<Self, ActiveReblitBootRenderInputsError> {
        let mut audited_packages = Vec::new();
        audited_packages
            .try_reserve_exact(packages.entries().len())
            .map_err(|source| allocation("audited package command lines", source))?;
        for entry in packages.entries() {
            require_deadline(
                deadline,
                "package command-line grammar audit",
                std::time::Instant::now(),
            )?;
            let source = ActiveReblitCmdlineSource::Package {
                binding_index: entry.binding_index(),
            };
            audit_snippet(entry.snippet(), source, deadline)?;
            audited_packages.push(AuditedPackageCmdline { entry });
        }

        let mut masks = Vec::new();
        let mut local_appends = Vec::new();
        masks
            .try_reserve_exact(local_policy.entries().len())
            .map_err(|source| allocation("local command-line masks", source))?;
        local_appends
            .try_reserve_exact(local_policy.entries().len())
            .map_err(|source| allocation("audited local command lines", source))?;
        for (entry_index, entry) in local_policy.entries().enumerate() {
            require_deadline(deadline, "local command-line grammar audit", std::time::Instant::now())?;
            match entry {
                BoundActiveReblitLocalCmdlineEntry::Append { name: _, snippet } => {
                    let entry_index = u16::try_from(entry_index).expect("local policy is bounded below u16::MAX");
                    audit_snippet(
                        snippet,
                        ActiveReblitCmdlineSource::LocalAppend { entry_index },
                        deadline,
                    )?;
                    local_appends.push(AuditedLocalAppend { snippet });
                }
                BoundActiveReblitLocalCmdlineEntry::Mask { name } => masks.push(name),
            }
        }
        Ok(Self {
            packages: audited_packages,
            masks,
            local_appends,
        })
    }

    fn package_is_masked(&self, filename: &OsStr) -> bool {
        self.masks.contains(&filename)
    }
}

pub(super) fn materialize_kernel_cmdline(
    audited: &AuditedCmdlineInputs<'_>,
    state_id: state::Id,
    version: &str,
    root_argument: &str,
    policy: BootRenderInputPolicy,
    aggregate_bytes: &mut usize,
    aggregate_tokens: &mut usize,
    deadline: std::time::Instant,
) -> Result<MaterializedKernelCmdline, ActiveReblitBootRenderInputsError> {
    materialize_kernel_cmdline_with_checkpoint(
        audited,
        state_id,
        version,
        root_argument,
        policy,
        aggregate_bytes,
        aggregate_tokens,
        deadline,
        |_, _| {},
    )
}

#[allow(clippy::too_many_arguments)]
fn materialize_kernel_cmdline_with_checkpoint<F>(
    audited: &AuditedCmdlineInputs<'_>,
    state_id: state::Id,
    version: &str,
    root_argument: &str,
    policy: BootRenderInputPolicy,
    aggregate_bytes: &mut usize,
    aggregate_tokens: &mut usize,
    deadline: std::time::Instant,
    mut admitted: F,
) -> Result<MaterializedKernelCmdline, ActiveReblitBootRenderInputsError>
where
    F: FnMut(usize, usize),
{
    validate_root_argument(root_argument)?;
    let mut cast_argument = String::new();
    cast_argument
        .try_reserve_exact("cast.fstx=".len() + 11)
        .map_err(|source| allocation("cast state token", source))?;
    write!(&mut cast_argument, "cast.fstx={}", i32::from(state_id)).map_err(|_| {
        ActiveReblitBootRenderInputsError::InvalidStateToken {
            state: i32::from(state_id),
        }
    })?;

    let mut selected = Vec::new();
    selected
        .try_reserve_exact(policy.max_cmdline_tokens.saturating_add(1).min(1_025))
        .map_err(|source| allocation("kernel semantic token references", source))?;
    let mut token_bytes = 0usize;
    for (package_index, package) in audited.packages.iter().enumerate() {
        require_deadline(
            deadline,
            "package command-line applicability",
            std::time::Instant::now(),
        )?;
        let entry = package.entry;
        let applicable = entry.state_id() == state_id
            && match entry.scope() {
                BoundActiveReblitPackageCmdlineScope::Global => true,
                BoundActiveReblitPackageCmdlineScope::Kernel { version: candidate } => candidate == version,
            };
        if !applicable || audited.package_is_masked(entry.filename()) {
            continue;
        }
        push_snippet_tokens(
            &mut selected,
            entry.snippet(),
            SnippetOwner::Package(u16::try_from(package_index).expect("package input count fits u16")),
            &mut token_bytes,
            state_id,
            version,
            policy,
            deadline,
        )?;
    }
    for (local_index, local) in audited.local_appends.iter().enumerate() {
        require_deadline(deadline, "local command-line append", std::time::Instant::now())?;
        push_snippet_tokens(
            &mut selected,
            local.snippet,
            SnippetOwner::Local(u16::try_from(local_index).expect("local append count fits u16")),
            &mut token_bytes,
            state_id,
            version,
            policy,
            deadline,
        )?;
    }
    push_token(
        &mut selected,
        SelectedToken::Root,
        root_argument.len(),
        &mut token_bytes,
        state_id,
        version,
        policy,
    )?;
    push_token(
        &mut selected,
        SelectedToken::Cast,
        cast_argument.len(),
        &mut token_bytes,
        state_id,
        version,
        policy,
    )?;

    let separator_bytes = selected.len().saturating_sub(1);
    let cmdline_bytes = token_bytes.checked_add(separator_bytes).unwrap_or(usize::MAX);
    if cmdline_bytes > policy.max_cmdline_bytes {
        return Err(ActiveReblitBootRenderInputsError::KernelCmdlineByteLimit {
            state: i32::from(state_id),
            version: version.to_owned().into_boxed_str(),
            limit: policy.max_cmdline_bytes,
            actual: cmdline_bytes,
        });
    }
    let prospective_tokens = aggregate_tokens.checked_add(selected.len()).unwrap_or(usize::MAX);
    if prospective_tokens > policy.max_total_cmdline_tokens {
        return Err(ActiveReblitBootRenderInputsError::AggregateCmdlineTokenLimit {
            limit: policy.max_total_cmdline_tokens,
            actual: prospective_tokens,
        });
    }
    let prospective_bytes = aggregate_bytes.checked_add(cmdline_bytes).unwrap_or(usize::MAX);
    if prospective_bytes > policy.max_total_cmdline_bytes {
        return Err(ActiveReblitBootRenderInputsError::AggregateCmdlineByteLimit {
            limit: policy.max_total_cmdline_bytes,
            actual: prospective_bytes,
        });
    }

    // Every authored byte, token, separator, and aggregate byte is admitted
    // before allocating or cloning any authored token into canonical output.
    admitted(cmdline_bytes, selected.len());
    let mut cmdline = String::new();
    cmdline
        .try_reserve_exact(cmdline_bytes)
        .map_err(|source| allocation("canonical kernel command line", source))?;
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(selected.len())
        .map_err(|source| allocation("canonical kernel token ranges", source))?;
    for token in selected {
        if !cmdline.is_empty() {
            cmdline.push(' ');
        }
        let start = cmdline.len();
        match token {
            SelectedToken::Package { index, start, end } => {
                let entry = audited.packages[usize::from(index)].entry;
                let snippet = entry.snippet();
                cmdline.push_str(&snippet[start as usize..end as usize]);
            }
            SelectedToken::Local { index, start, end } => {
                let snippet = audited.local_appends[usize::from(index)].snippet;
                cmdline.push_str(&snippet[start as usize..end as usize]);
            }
            SelectedToken::Root => cmdline.push_str(root_argument),
            SelectedToken::Cast => cmdline.push_str(&cast_argument),
        }
        let end = cmdline.len();
        ranges.push(CmdlineTokenRange {
            start: u16::try_from(start).expect("per-kernel command-line bound fits u16"),
            end: u16::try_from(end).expect("per-kernel command-line bound fits u16"),
        });
    }
    *aggregate_tokens = prospective_tokens;
    *aggregate_bytes = prospective_bytes;
    require_deadline(
        deadline,
        "canonical command-line materialization",
        std::time::Instant::now(),
    )?;
    Ok(MaterializedKernelCmdline {
        cmdline: cmdline.into_boxed_str(),
        tokens: ranges.into_boxed_slice(),
    })
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn materialize_kernel_cmdline_with_admission_checkpoint<F>(
    audited: &AuditedCmdlineInputs<'_>,
    state_id: state::Id,
    version: &str,
    root_argument: &str,
    policy: BootRenderInputPolicy,
    aggregate_bytes: &mut usize,
    aggregate_tokens: &mut usize,
    deadline: std::time::Instant,
    mut checkpoint: F,
) -> Result<MaterializedKernelCmdline, ActiveReblitBootRenderInputsError>
where
    F: FnMut(CmdlineMaterializationCheckpoint),
{
    materialize_kernel_cmdline_with_checkpoint(
        audited,
        state_id,
        version,
        root_argument,
        policy,
        aggregate_bytes,
        aggregate_tokens,
        deadline,
        |bytes, tokens| checkpoint(CmdlineMaterializationCheckpoint::Admitted { bytes, tokens }),
    )
}

pub(super) fn validate_root_argument(root_argument: &str) -> Result<(), ActiveReblitBootRenderInputsError> {
    let root_bytes = root_argument.as_bytes();
    if !root_argument.starts_with("root=")
        || root_argument.len() == "root=".len()
        || root_bytes.iter().any(|byte| !(0x21..=0x7e).contains(byte))
        || root_bytes.iter().any(|byte| matches!(byte, b'\'' | b'"' | b'\\'))
    {
        Err(ActiveReblitBootRenderInputsError::InvalidRootArgument)
    } else {
        Ok(())
    }
}

fn audit_snippet(
    snippet: &str,
    source: ActiveReblitCmdlineSource,
    deadline: std::time::Instant,
) -> Result<(), ActiveReblitBootRenderInputsError> {
    for token in snippet.split(' ').filter(|token| !token.is_empty()) {
        require_deadline(deadline, "command-line token grammar audit", std::time::Instant::now())?;
        let bytes = token.as_bytes();
        if bytes.iter().any(|byte| !(0x20..=0x7e).contains(byte)) {
            return Err(ActiveReblitBootRenderInputsError::InvalidCmdlineToken {
                origin: source,
                reason: ActiveReblitCmdlineTokenReason::NonPrintableAscii,
            });
        }
        if token == "--" {
            return Err(ActiveReblitBootRenderInputsError::InvalidCmdlineToken {
                origin: source,
                reason: ActiveReblitCmdlineTokenReason::EndOfOptionsSeparator,
            });
        }
        let key = token.split_once('=').map_or(token, |(key, _)| key);
        if key.is_empty() {
            return Err(ActiveReblitBootRenderInputsError::InvalidCmdlineToken {
                origin: source,
                reason: ActiveReblitCmdlineTokenReason::EmptyKey,
            });
        }
        if key == "root" || key == "cast.fstx" {
            return Err(ActiveReblitBootRenderInputsError::ReservedCmdlineKey {
                origin: source,
                key: if key == "root" { "root" } else { "cast.fstx" },
            });
        }
        if bytes.iter().any(|byte| matches!(byte, b'\'' | b'"' | b'\\')) {
            return Err(ActiveReblitBootRenderInputsError::InvalidCmdlineToken {
                origin: source,
                reason: ActiveReblitCmdlineTokenReason::UnsupportedQuoteOrEscape,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn audit_snippet_for_test(
    snippet: &str,
    source: ActiveReblitCmdlineSource,
    deadline: std::time::Instant,
) -> Result<(), ActiveReblitBootRenderInputsError> {
    audit_snippet(snippet, source, deadline)
}

#[allow(clippy::too_many_arguments)]
fn push_snippet_tokens(
    selected: &mut Vec<SelectedToken>,
    snippet: &str,
    owner: SnippetOwner,
    token_bytes: &mut usize,
    state_id: state::Id,
    version: &str,
    policy: BootRenderInputPolicy,
    deadline: std::time::Instant,
) -> Result<(), ActiveReblitBootRenderInputsError> {
    let mut start = 0usize;
    for token in snippet.split(' ') {
        require_deadline(deadline, "command-line token selection", std::time::Instant::now())?;
        let end = start.saturating_add(token.len());
        if !token.is_empty() {
            let start_u32 = u32::try_from(start).expect("authenticated command-line source bound fits u32");
            let end_u32 = u32::try_from(end).expect("authenticated command-line source bound fits u32");
            let coordinate = match owner {
                SnippetOwner::Package(index) => SelectedToken::Package {
                    index,
                    start: start_u32,
                    end: end_u32,
                },
                SnippetOwner::Local(index) => SelectedToken::Local {
                    index,
                    start: start_u32,
                    end: end_u32,
                },
            };
            push_token(
                selected,
                coordinate,
                token.len(),
                token_bytes,
                state_id,
                version,
                policy,
            )?;
        }
        start = end.saturating_add(1);
    }
    Ok(())
}

fn push_token(
    selected: &mut Vec<SelectedToken>,
    token: SelectedToken,
    token_length: usize,
    token_bytes: &mut usize,
    state_id: state::Id,
    version: &str,
    policy: BootRenderInputPolicy,
) -> Result<(), ActiveReblitBootRenderInputsError> {
    let actual = selected.len().saturating_add(1);
    if actual > policy.max_cmdline_tokens {
        return Err(ActiveReblitBootRenderInputsError::KernelCmdlineTokenLimit {
            state: i32::from(state_id),
            version: version.to_owned().into_boxed_str(),
            limit: policy.max_cmdline_tokens,
            actual,
        });
    }
    *token_bytes = token_bytes.checked_add(token_length).ok_or_else(|| {
        ActiveReblitBootRenderInputsError::KernelCmdlineByteLimit {
            state: i32::from(state_id),
            version: version.to_owned().into_boxed_str(),
            limit: policy.max_cmdline_bytes,
            actual: usize::MAX,
        }
    })?;
    selected.push(token);
    Ok(())
}
