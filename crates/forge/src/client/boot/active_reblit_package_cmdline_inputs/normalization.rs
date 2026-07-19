use super::{ActiveReblitPackageCmdlineContentReason, ActiveReblitPackageCmdlineInputsError, PackageCmdlineBudget};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PackageCmdlineNormalizationCheckpoint {
    Materialized,
}

pub(super) fn normalize_package_cmdline(
    binding_index: usize,
    bytes: &[u8],
    budget: &mut PackageCmdlineBudget,
) -> Result<Box<str>, ActiveReblitPackageCmdlineInputsError> {
    normalize_package_cmdline_with_checkpoint(binding_index, bytes, budget, |_| {})
}

pub(super) fn normalize_package_cmdline_with_checkpoint<F>(
    binding_index: usize,
    bytes: &[u8],
    budget: &mut PackageCmdlineBudget,
    mut checkpoint: F,
) -> Result<Box<str>, ActiveReblitPackageCmdlineInputsError>
where
    F: FnMut(PackageCmdlineNormalizationCheckpoint),
{
    budget.step("command-line syntax admission")?;
    if bytes
        .iter()
        .any(|byte| !byte.is_ascii() || (byte.is_ascii_control() && !matches!(byte, b'\n' | b'\r' | b'\t')))
    {
        return Err(ActiveReblitPackageCmdlineInputsError::InvalidContent {
            binding_index,
            reason: ActiveReblitPackageCmdlineContentReason::NonAsciiOrUnsupportedControl,
        });
    }

    let source = std::str::from_utf8(bytes).expect("admitted ASCII is UTF-8");
    let mut normalized = String::new();
    normalized
        .try_reserve_exact(bytes.len())
        .map_err(|source| ActiveReblitPackageCmdlineInputsError::Allocation {
            resource: "normalized package command-line bytes",
            source,
        })?;

    let mut first = true;
    for line in source.lines().map(str::trim).filter(|line| !line.starts_with('#')) {
        budget.step("command-line line normalization")?;
        if !first {
            normalized.push(' ');
        }
        first = false;
        normalized.push_str(line);
    }

    if normalized.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ActiveReblitPackageCmdlineInputsError::InvalidContent {
            binding_index,
            reason: ActiveReblitPackageCmdlineContentReason::NormalizedControl,
        });
    }
    let normalized = normalized.into_boxed_str();
    checkpoint(PackageCmdlineNormalizationCheckpoint::Materialized);
    budget.require_deadline("normalized command-line materialization")?;
    Ok(normalized)
}
