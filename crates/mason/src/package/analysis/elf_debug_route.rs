use stone_recipe::{
    build_policy::AnalyzerKind,
    derivation::AnalysisPlan,
};

use super::{
    BoxError,
    handler::{
        VerifiedElf, blocked_reason, compressman_candidate, is_elf_candidate, parse_build_id,
        pending_debug_destination,
    },
};
use crate::package::collect::{Collector, PathInfo, ProjectedPathKind};

/// Authenticate every initial ELF that can request a generated build-ID debug
/// file and resolve that projected file before any analyzer is constructed.
pub(in crate::package) fn preflight_elf_debug_routes(
    analysis: &AnalysisPlan,
    collector: &Collector,
    paths: &[PathInfo],
) -> Result<(), BoxError> {
    if !analysis.debug {
        return Ok(());
    }

    for info in paths {
        info.check_deadline()?;
        if !elf_handler_reachable(analysis, info) || !is_elf_candidate(info) {
            continue;
        }
        let Some(mut verified_elf) = VerifiedElf::from_path_info(info)? else {
            continue;
        };
        let bit_size = verified_elf.stream_mut().ehdr.class;
        let Some(build_id) = parse_build_id(verified_elf.stream_mut()) else {
            continue;
        };
        let Some(destination) = pending_debug_destination(info, bit_size, &build_id)? else {
            continue;
        };
        collector.projected_package_for(&destination, ProjectedPathKind::Regular { mode: 0o644 })?;
        info.check_deadline()?;
    }
    Ok(())
}

fn elf_handler_reachable(analysis: &AnalysisPlan, info: &PathInfo) -> bool {
    for handler in &analysis.handlers {
        match handler {
            AnalyzerKind::Elf => return true,
            AnalyzerKind::IgnoreBlocked if blocked_reason(analysis, info).is_some() => return false,
            AnalyzerKind::CompressMan if compressman_candidate(analysis, info) => return false,
            AnalyzerKind::IncludeAny => return false,
            AnalyzerKind::IgnoreBlocked
            | AnalyzerKind::Binary
            | AnalyzerKind::PkgConfig
            | AnalyzerKind::Python
            | AnalyzerKind::CMake
            | AnalyzerKind::CompressMan => {}
        }
    }
    false
}

#[cfg(test)]
mod tests;
