use std::{
    fs::Metadata,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    sync::Arc,
};

use glob::Pattern;
use stone_recipe::derivation::PathRuleKind;

use super::{CollectionContext, Collector, Error};

/// One ordered package-output routing rule.
#[derive(Debug)]
pub struct Rule {
    pattern: String,
    pub(super) package: Arc<str>,
    kind: PathRuleKind,
    exact: Pattern,
    descendant: Pattern,
}

pub(super) struct CompiledRule {
    pattern: String,
    kind: PathRuleKind,
    exact: Pattern,
    descendant: Pattern,
}

impl Rule {
    pub(super) fn compile(
        pattern: String,
        descendant_pattern: String,
        kind: PathRuleKind,
    ) -> Result<CompiledRule, Error> {
        let exact = Pattern::new(&pattern).map_err(|source| Error::InvalidRulePattern {
            pattern: pattern.clone(),
            detail: source.to_string(),
        })?;
        let descendant = Pattern::new(&descendant_pattern).map_err(|source| Error::InvalidRulePattern {
            pattern: descendant_pattern,
            detail: source.to_string(),
        })?;
        Ok(CompiledRule {
            pattern,
            kind,
            exact,
            descendant,
        })
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    pub fn package(&self) -> &str {
        &self.package
    }

    pub fn kind(&self) -> PathRuleKind {
        self.kind
    }

    fn matches(&self, target: &str, kind: RoutedPathKind) -> bool {
        let pattern_matches = self.pattern == target || self.exact.matches(target) || self.descendant.matches(target);
        pattern_matches
            && match self.kind {
                PathRuleKind::Any => true,
                PathRuleKind::Executable => matches!(kind, RoutedPathKind::Regular { mode } if mode & 0o111 != 0),
                PathRuleKind::Symlink => kind == RoutedPathKind::Symlink,
                PathRuleKind::Special => kind == RoutedPathKind::Special,
            }
    }
}

impl CompiledRule {
    pub(super) fn bind_package(self, package: Arc<str>) -> Rule {
        Rule {
            pattern: self.pattern,
            package,
            kind: self.kind,
            exact: self.exact,
            descendant: self.descendant,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoutedPathKind {
    Regular { mode: u32 },
    Directory,
    Symlink,
    Special,
    Other,
}

impl RoutedPathKind {
    fn from_metadata(metadata: &Metadata) -> Self {
        let file_type = metadata.file_type();
        if file_type.is_file() {
            Self::Regular { mode: metadata.mode() }
        } else if file_type.is_dir() {
            Self::Directory
        } else if file_type.is_symlink() {
            Self::Symlink
        } else if file_type.is_char_device()
            || file_type.is_block_device()
            || file_type.is_fifo()
            || file_type.is_socket()
        {
            Self::Special
        } else {
            Self::Other
        }
    }
}

/// Filesystem kind and mode that a generated artifact will have after its
/// private inode is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectedPathKind {
    Regular { mode: u32 },
    Symlink,
}

impl From<ProjectedPathKind> for RoutedPathKind {
    fn from(kind: ProjectedPathKind) -> Self {
        match kind {
            ProjectedPathKind::Regular { mode } => Self::Regular { mode },
            ProjectedPathKind::Symlink => Self::Symlink,
        }
    }
}

impl Collector {
    fn matching_package(
        &self,
        context: &CollectionContext,
        target: &str,
        kind: RoutedPathKind,
        display_path: &Path,
    ) -> Result<Option<Arc<str>>, Error> {
        for rule in self.rules.iter().rev() {
            context.check_time(display_path)?;
            if rule.matches(target, kind) {
                context.check_time(display_path)?;
                return Ok(Some(Arc::clone(&rule.package)));
            }
        }
        context.check_time(display_path)?;
        Ok(None)
    }

    fn package_for_kind(
        &self,
        context: &CollectionContext,
        relative: &Path,
        kind: RoutedPathKind,
        display_path: &Path,
    ) -> Result<Arc<str>, Error> {
        let target_path = Path::new("/").join(relative);
        let target = target_path.to_str().ok_or_else(|| Error::NonUtf8Path {
            path: display_path.to_owned(),
        })?;
        self.matching_package(context, target, kind, display_path)?
            .ok_or_else(|| Error::NoMatchingRule {
                path: display_path.to_owned(),
            })
    }

    pub(super) fn package_for(
        &self,
        context: &CollectionContext,
        relative: &Path,
        metadata: &Metadata,
        display_path: &Path,
    ) -> Result<Arc<str>, Error> {
        self.package_for_kind(
            context,
            relative,
            RoutedPathKind::from_metadata(metadata),
            display_path,
        )
    }

    pub(super) fn package_for_projected(
        &self,
        context: &CollectionContext,
        relative: &Path,
        kind: ProjectedPathKind,
        display_path: &Path,
    ) -> Result<Arc<str>, Error> {
        self.package_for_kind(context, relative, kind.into(), display_path)
    }
}

#[cfg(test)]
mod tests;
