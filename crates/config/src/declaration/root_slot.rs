use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsStr,
    fmt, io,
    path::{Component, Path, PathBuf},
};

use declarative_config::LanguageSpec;
use fs_err as fs;

/// Immutable set of declaration languages available to discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredLanguages {
    by_extension: BTreeMap<String, LanguageSpec>,
}

impl RegisteredLanguages {
    pub fn new(
        languages: impl IntoIterator<Item = LanguageSpec>,
    ) -> Result<Self, LanguageRegistrationError> {
        let mut by_extension: BTreeMap<String, LanguageSpec> = BTreeMap::new();
        for language in languages {
            let extension = language.extension().to_owned();
            if let Some(existing) = by_extension.get(&extension) {
                return Err(LanguageRegistrationError {
                    extension,
                    first_language: existing.language().as_str().to_owned(),
                    duplicate_language: language.language().as_str().to_owned(),
                });
            }
            by_extension.insert(extension, language);
        }
        Ok(Self { by_extension })
    }

    pub fn len(&self) -> usize {
        self.by_extension.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_extension.is_empty()
    }

    pub fn get(&self, extension: &str) -> Option<&LanguageSpec> {
        self.by_extension.get(extension)
    }

    pub fn iter(&self) -> impl Iterator<Item = &LanguageSpec> {
        self.by_extension.values()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageRegistrationError {
    extension: String,
    first_language: String,
    duplicate_language: String,
}

impl LanguageRegistrationError {
    pub fn extension(&self) -> &str {
        &self.extension
    }

    pub fn first_language(&self) -> &str {
        &self.first_language
    }

    pub fn duplicate_language(&self) -> &str {
        &self.duplicate_language
    }
}

impl fmt::Display for LanguageRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "declaration extension {:?} is registered by both {:?} and {:?}",
            self.extension, self.first_language, self.duplicate_language
        )
    }
}

impl Error for LanguageRegistrationError {}

/// One fixed declaration slot, independent of any evaluator implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootDeclarationSlot {
    basename: String,
    logical_name: String,
}

impl RootDeclarationSlot {
    pub fn new(
        basename: impl Into<String>,
        logical_name: impl Into<String>,
    ) -> Result<Self, RootDeclarationSlotError> {
        let basename = basename.into();
        if !is_safe_basename(&basename) {
            return Err(RootDeclarationSlotError::InvalidBasename { basename });
        }
        let logical_name = logical_name.into();
        if logical_name.is_empty() || logical_name.contains('\0') {
            return Err(RootDeclarationSlotError::InvalidLogicalName { logical_name });
        }
        Ok(Self {
            basename,
            logical_name,
        })
    }

    pub fn basename(&self) -> &str {
        &self.basename
    }

    pub fn logical_name(&self) -> &str {
        &self.logical_name
    }

    /// Discover the exact registered-language file occupying this slot.
    ///
    /// Discovery performs metadata checks only. It does not enumerate the
    /// directory, inspect file contents, or select an evaluator.
    pub fn discover(
        &self,
        directory: &Path,
        languages: &RegisteredLanguages,
    ) -> Result<Option<DiscoveredRootDeclaration>, RootDeclarationDiscoveryError> {
        let mut candidates = Vec::new();
        for language in languages.iter() {
            let relative_path = PathBuf::from(format!(
                "{}.{}",
                self.basename,
                language.extension()
            ));
            let path = directory.join(&relative_path);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(RootDeclarationDiscoveryError::Inspect {
                        path,
                        source,
                    });
                }
            };
            if !metadata.file_type().is_file() {
                return Err(RootDeclarationDiscoveryError::NotRegular { path });
            }
            candidates.push(DiscoveredRootDeclaration {
                path,
                relative_path,
                language: language.clone(),
                logical_name: self.logical_name.clone(),
            });
        }

        match candidates.len() {
            0 => Ok(None),
            1 => Ok(candidates.pop()),
            _ => Err(RootDeclarationDiscoveryError::Collision {
                logical_name: self.logical_name.clone(),
                paths: candidates
                    .into_iter()
                    .map(|candidate| candidate.path)
                    .collect(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootDeclarationSlotError {
    InvalidBasename { basename: String },
    InvalidLogicalName { logical_name: String },
}

impl fmt::Display for RootDeclarationSlotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBasename { basename } => {
                write!(formatter, "invalid declaration basename {basename:?}")
            }
            Self::InvalidLogicalName { logical_name } => {
                write!(formatter, "invalid declaration logical name {logical_name:?}")
            }
        }
    }
}

impl Error for RootDeclarationSlotError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRootDeclaration {
    path: PathBuf,
    relative_path: PathBuf,
    language: LanguageSpec,
    logical_name: String,
}

impl DiscoveredRootDeclaration {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub fn language(&self) -> &LanguageSpec {
        &self.language
    }

    pub fn logical_name(&self) -> &str {
        &self.logical_name
    }
}

#[derive(Debug)]
pub enum RootDeclarationDiscoveryError {
    Inspect { path: PathBuf, source: io::Error },
    NotRegular { path: PathBuf },
    Collision {
        logical_name: String,
        paths: Vec<PathBuf>,
    },
}

impl RootDeclarationDiscoveryError {
    pub fn collision_paths(&self) -> Option<&[PathBuf]> {
        match self {
            Self::Collision { paths, .. } => Some(paths),
            _ => None,
        }
    }
}

impl fmt::Display for RootDeclarationDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspect { path, .. } => {
                write!(formatter, "inspect declaration candidate {}", path.display())
            }
            Self::NotRegular { path } => {
                write!(formatter, "declaration candidate {} is not a regular file", path.display())
            }
            Self::Collision {
                logical_name,
                paths,
            } => write!(
                formatter,
                "declaration slot {logical_name:?} has {} registered-language candidates",
                paths.len()
            ),
        }
    }
}

impl Error for RootDeclarationDiscoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Inspect { source, .. } => Some(source),
            Self::NotRegular { .. } | Self::Collision { .. } => None,
        }
    }
}

fn is_safe_basename(basename: &str) -> bool {
    if basename.is_empty()
        || basename.contains('\\')
        || basename.chars().any(char::is_control)
    {
        return false;
    }
    let mut components = Path::new(basename).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(component)), None) if component == OsStr::new(basename)
    )
}

#[cfg(test)]
mod tests {
    use declarative_config::{EngineId, LanguageId};
    use tempfile::tempdir;

    use super::*;

    fn language(name: &str, extension: &str) -> LanguageSpec {
        LanguageSpec::new(
            LanguageId::new(name).unwrap(),
            EngineId::new(format!("{name}-engine"), "1").unwrap(),
            extension,
            "declaration-v1",
            format!("# generated by {name}\n"),
        )
        .unwrap()
    }

    fn languages() -> RegisteredLanguages {
        RegisteredLanguages::new([
            language("fixture", "decl"),
            language("alternate", "alt"),
        ])
        .unwrap()
    }

    #[test]
    fn registration_is_immutable_and_rejects_duplicate_extensions() {
        let languages = languages();
        assert_eq!(languages.len(), 2);
        assert_eq!(languages.get("decl").unwrap().language().as_str(), "fixture");

        let error = RegisteredLanguages::new([
            language("fixture", "decl"),
            language("alternate", "decl"),
        ])
        .unwrap_err();
        assert_eq!(error.extension(), "decl");
        assert_eq!(error.first_language(), "fixture");
        assert_eq!(error.duplicate_language(), "alternate");
    }

    #[test]
    fn exact_registered_candidate_carries_physical_and_logical_identity() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("system.decl"), "fixture").unwrap();
        fs::write(directory.path().join("system.unknown"), "ignored").unwrap();
        fs::write(directory.path().join("nearby.alt"), "ignored").unwrap();
        let slot = RootDeclarationSlot::new("system", "etc/cast/system").unwrap();

        let found = slot.discover(directory.path(), &languages()).unwrap().unwrap();

        assert_eq!(found.path(), directory.path().join("system.decl"));
        assert_eq!(found.relative_path(), Path::new("system.decl"));
        assert_eq!(found.language().extension(), "decl");
        assert_eq!(found.logical_name(), "etc/cast/system");
    }

    #[test]
    fn unknown_extensions_are_ignored_and_absence_is_none() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("system.unknown"), "not registered").unwrap();
        let slot = RootDeclarationSlot::new("system", "system").unwrap();

        assert_eq!(slot.discover(directory.path(), &languages()).unwrap(), None);
    }

    #[test]
    fn multiple_registered_candidates_are_a_structured_collision() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("system.decl"), "fixture").unwrap();
        fs::write(directory.path().join("system.alt"), "alternate").unwrap();
        let slot = RootDeclarationSlot::new("system", "system").unwrap();

        let error = slot.discover(directory.path(), &languages()).unwrap_err();

        assert_eq!(
            error.collision_paths().unwrap(),
            &[
                directory.path().join("system.alt"),
                directory.path().join("system.decl"),
            ]
        );
    }

    #[test]
    fn matching_non_regular_entries_fail_closed() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("system.decl")).unwrap();
        let slot = RootDeclarationSlot::new("system", "system").unwrap();

        assert!(matches!(
            slot.discover(directory.path(), &languages()),
            Err(RootDeclarationDiscoveryError::NotRegular { .. })
        ));
    }

    #[test]
    fn slot_names_are_explicit_safe_and_normalized() {
        let slot = RootDeclarationSlot::new("boot-topology", "etc/cast/boot-topology").unwrap();
        assert_eq!(slot.basename(), "boot-topology");
        assert_eq!(slot.logical_name(), "etc/cast/boot-topology");

        for basename in ["", ".", "..", "nested/name", "nested\\name", "line\nbreak"] {
            assert!(RootDeclarationSlot::new(basename, "logical").is_err(), "{basename:?}");
        }
        assert!(RootDeclarationSlot::new("system", "").is_err());
    }
}
