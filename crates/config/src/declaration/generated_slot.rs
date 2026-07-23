use std::{ffi::OsString, io, path::{Component, Path, PathBuf}};

use declarative_config::LanguageSpec;
use fs_err as fs;

use super::{
    atomic_persistence::{
        AtomicWrite, ExpectedTarget, require_same_generated_declaration,
    },
    managed_directory::{ManagedDirectory, inspect_existing_declaration},
    storage_error::{
        DeleteDeclarationError, GeneratedDeclarationSlotError,
        SaveDeclarationError,
    },
};

const MAX_FILE_NAME_BYTES: usize = 255;
const TEMPORARY_RANDOM_HEX_BYTES: usize = 32;

/// Exclusive authority for one generated declaration path.
///
/// Language selection and ownership are explicit. The marker is used only to
/// distinguish generated content from authored content; it never detects a
/// language or selects an evaluator.
#[derive(Debug, Clone)]
pub struct GeneratedDeclarationSlot {
    directory: PathBuf,
    name: String,
    language: LanguageSpec,
    ownership_marker: Vec<u8>,
    size_limit: usize,
    temporary_prefix: String,
}

impl GeneratedDeclarationSlot {
    pub fn new(
        directory: impl Into<PathBuf>,
        name: impl Into<String>,
        language: LanguageSpec,
        ownership_marker: impl Into<Vec<u8>>,
        size_limit: usize,
        temporary_prefix: impl Into<String>,
    ) -> Result<Self, GeneratedDeclarationSlotError> {
        let name = name.into();
        let file_name_bytes = name
            .len()
            .saturating_add(1)
            .saturating_add(language.extension().len());
        if !is_safe_component(&name) || file_name_bytes > MAX_FILE_NAME_BYTES {
            return Err(GeneratedDeclarationSlotError::InvalidName { name });
        }
        let ownership_marker = ownership_marker.into();
        if ownership_marker.is_empty() {
            return Err(GeneratedDeclarationSlotError::InvalidOwnershipMarker);
        }
        let temporary_prefix = temporary_prefix.into();
        if !is_safe_component(&temporary_prefix)
            || temporary_prefix
                .len()
                .saturating_add(TEMPORARY_RANDOM_HEX_BYTES)
                > MAX_FILE_NAME_BYTES
        {
            return Err(GeneratedDeclarationSlotError::InvalidTemporaryPrefix {
                prefix: temporary_prefix,
            });
        }
        if size_limit == 0 {
            return Err(GeneratedDeclarationSlotError::ZeroSizeLimit);
        }
        Ok(Self {
            directory: directory.into(),
            name,
            language,
            ownership_marker,
            size_limit,
            temporary_prefix,
        })
    }

    pub fn language_spec(&self) -> &LanguageSpec {
        &self.language
    }

    pub fn path(&self) -> PathBuf {
        self.directory.join(self.file_name())
    }

    pub fn ownership_marker(&self) -> &[u8] {
        &self.ownership_marker
    }

    pub fn save(&self, bytes: &[u8]) -> Result<PathBuf, SaveDeclarationError> {
        self.save_with_hook(bytes, |_| {})
    }

    pub(crate) fn save_with_hook(
        &self,
        bytes: &[u8],
        before_commit: impl FnOnce(&Path),
    ) -> Result<PathBuf, SaveDeclarationError> {
        let path = self.path();
        if !bytes.starts_with(&self.ownership_marker) {
            return Err(SaveDeclarationError::MissingOwnershipMarker { path });
        }
        if bytes.len() > self.size_limit {
            return Err(SaveDeclarationError::GeneratedTooLarge {
                size: bytes.len(),
                limit: self.size_limit,
            });
        }
        fs::create_dir_all(&self.directory).map_err(|source| {
            SaveDeclarationError::CreateDirectory {
                path: self.directory.clone(),
                source,
            }
        })?;
        let directory = ManagedDirectory::open(&self.directory).map_err(|source| {
            SaveDeclarationError::CreateDirectory {
                path: self.directory.clone(),
                source,
            }
        })?;
        directory.verify_path().map_err(|source| {
            SaveDeclarationError::CreateDirectory {
                path: self.directory.clone(),
                source,
            }
        })?;
        let file_name = self.file_name();
        let expected = match inspect_existing_declaration(
            &directory,
            &file_name,
            self.size_limit,
            &self.ownership_marker,
        )
        .map_err(|source| SaveDeclarationError::ReadExisting {
            path: path.clone(),
            source,
        })? {
            None => ExpectedTarget::Missing,
            Some(existing) if existing.is_generated() => {
                ExpectedTarget::Generated(existing.identity())
            }
            Some(_) => {
                return Err(SaveDeclarationError::AuthoredDeclaration { path });
            }
        };
        super::atomic_persistence::atomic_write_with_hook(
            AtomicWrite {
                directory: &directory,
                file_name: &file_name,
                path: &path,
                bytes,
                expected,
                size_limit: self.size_limit,
                ownership_marker: &self.ownership_marker,
                temporary_prefix: &self.temporary_prefix,
            },
            before_commit,
        )?;
        Ok(path)
    }

    pub fn delete(&self) -> Result<(), DeleteDeclarationError> {
        self.delete_with_hook(|| {})
    }

    pub(crate) fn delete_with_hook(
        &self,
        before_remove: impl FnOnce(),
    ) -> Result<(), DeleteDeclarationError> {
        let path = self.path();
        let directory = match ManagedDirectory::open(&self.directory) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(DeleteDeclarationError::ReadExisting { path, source });
            }
        };
        directory.verify_path().map_err(|source| {
            DeleteDeclarationError::ReadExisting {
                path: path.clone(),
                source,
            }
        })?;
        let file_name = self.file_name();
        let existing = match inspect_existing_declaration(
            &directory,
            &file_name,
            self.size_limit,
            &self.ownership_marker,
        )
        .map_err(|source| DeleteDeclarationError::ReadExisting {
            path: path.clone(),
            source,
        })? {
            None => return Ok(()),
            Some(existing) if existing.is_generated() => existing,
            Some(_) => {
                return Err(DeleteDeclarationError::AuthoredDeclaration { path });
            }
        };
        before_remove();
        directory.verify_path().map_err(|source| {
            DeleteDeclarationError::ReadExisting {
                path: path.clone(),
                source,
            }
        })?;
        require_same_generated_declaration(
            &directory,
            &file_name,
            existing.identity(),
            self.size_limit,
            &self.ownership_marker,
        )
        .map_err(|source| DeleteDeclarationError::ReadExisting {
            path: path.clone(),
            source,
        })?;
        directory
            .unlink(&file_name)
            .map_err(|source| DeleteDeclarationError::Remove {
                path: path.clone(),
                source,
            })?;
        directory
            .sync()
            .map_err(|source| DeleteDeclarationError::SyncDirectory {
                path: self.directory.clone(),
                source,
            })?;
        directory.verify_path().map_err(|source| {
            DeleteDeclarationError::SyncDirectory {
                path: self.directory.clone(),
                source,
            }
        })
    }

    fn file_name(&self) -> OsString {
        OsString::from(format!("{}.{}", self.name, self.language.extension()))
    }
}

fn is_safe_component(value: &str) -> bool {
    if value.is_empty()
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return false;
    }
    let mut components = Path::new(value).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(component)), None) if component == std::ffi::OsStr::new(value)
    )
}

#[cfg(test)]
mod tests {
    use declarative_config::{EngineId, LanguageId};

    use super::*;

    fn language(extension: &str) -> LanguageSpec {
        LanguageSpec::new(
            LanguageId::new("fixture").unwrap(),
            EngineId::new("fixture-engine", "1").unwrap(),
            extension,
            "fixture-v1",
            "# language default marker\n",
        )
        .unwrap()
    }

    fn slot(
        directory: &Path,
        name: &str,
        marker: &str,
    ) -> GeneratedDeclarationSlot {
        GeneratedDeclarationSlot::new(
            directory,
            name,
            language("decl"),
            marker,
            1024,
            ".fixture-tmp-",
        )
        .unwrap()
    }

    fn temporary_names(directory: &Path) -> Vec<OsString> {
        let mut names = fs::read_dir(directory)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| {
                name.to_string_lossy().starts_with(".fixture-tmp-")
            })
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn slot_preserves_exact_bytes_and_uses_explicit_language_extension() {
        let temporary = tempfile::tempdir().unwrap();
        let slot = slot(temporary.path(), "system", "# owned by system slot\n");
        let bytes = b"# owned by system slot\nvalue-without-final-newline";

        let path = slot.save(bytes).unwrap();

        assert_eq!(path, temporary.path().join("system.decl"));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(slot.language_spec().extension(), "decl");
        assert_eq!(slot.ownership_marker(), b"# owned by system slot\n");

        slot.delete().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn ownership_marker_is_slot_specific_not_a_language_detector() {
        let temporary = tempfile::tempdir().unwrap();
        let first = slot(temporary.path(), "snapshot", "# snapshot owner\n");
        let competing = slot(temporary.path(), "snapshot", "# other owner\n");
        first.save(b"# snapshot owner\nvalue").unwrap();

        let error = competing.save(b"# other owner\nreplacement").unwrap_err();

        assert!(matches!(
            error,
            SaveDeclarationError::AuthoredDeclaration { .. }
        ));
        assert_eq!(
            fs::read(first.path()).unwrap(),
            b"# snapshot owner\nvalue"
        );
    }

    #[test]
    fn authored_files_and_unmarked_generated_bytes_are_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let slot = slot(temporary.path(), "profile", "# generated profile\n");
        fs::write(slot.path(), "authored value\n").unwrap();

        assert!(matches!(
            slot.save(b"# generated profile\nreplacement"),
            Err(SaveDeclarationError::AuthoredDeclaration { .. })
        ));
        assert!(matches!(
            slot.delete(),
            Err(DeleteDeclarationError::AuthoredDeclaration { .. })
        ));

        fs::remove_file(slot.path()).unwrap();
        assert!(matches!(
            slot.save(b"missing marker"),
            Err(SaveDeclarationError::MissingOwnershipMarker { .. })
        ));
    }

    #[test]
    fn absent_target_race_does_not_replace_new_authored_content() {
        let temporary = tempfile::tempdir().unwrap();
        let slot = slot(temporary.path(), "race", "# generated race\n");
        let path = slot.path();

        let error = slot
            .save_with_hook(b"# generated race\nmanaged", |_| {
                fs::write(&path, "authored during save\n").unwrap();
            })
            .unwrap_err();

        assert!(matches!(error, SaveDeclarationError::Rename { .. }));
        assert_eq!(fs::read_to_string(path).unwrap(), "authored during save\n");
        assert!(temporary_names(temporary.path()).is_empty());
    }

    #[test]
    fn replacement_race_revalidates_the_generated_target() {
        let temporary = tempfile::tempdir().unwrap();
        let slot = slot(temporary.path(), "race", "# generated race\n");
        let path = slot.save(b"# generated race\nold").unwrap();

        let error = slot
            .save_with_hook(b"# generated race\nnew", |_| {
                fs::remove_file(&path).unwrap();
                fs::write(&path, "authored replacement\n").unwrap();
            })
            .unwrap_err();

        assert!(matches!(error, SaveDeclarationError::ReadExisting { .. }));
        assert_eq!(fs::read_to_string(path).unwrap(), "authored replacement\n");
        assert!(temporary_names(temporary.path()).is_empty());
    }

    #[test]
    fn slot_policy_rejects_unsafe_names_prefixes_markers_and_limits() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["", ".", "..", "nested/name", "nested\\name"] {
            assert!(matches!(
                GeneratedDeclarationSlot::new(
                    directory.path(),
                    name,
                    language("decl"),
                    "# marker\n",
                    1,
                    ".tmp-",
                ),
                Err(GeneratedDeclarationSlotError::InvalidName { .. })
            ));
        }
        assert!(matches!(
            GeneratedDeclarationSlot::new(
                directory.path(),
                "safe",
                language("decl"),
                Vec::new(),
                1,
                ".tmp-",
            ),
            Err(GeneratedDeclarationSlotError::InvalidOwnershipMarker)
        ));
        assert!(matches!(
            GeneratedDeclarationSlot::new(
                directory.path(),
                "safe",
                language("decl"),
                "# marker\n",
                0,
                ".tmp-",
            ),
            Err(GeneratedDeclarationSlotError::ZeroSizeLimit)
        ));
        assert!(matches!(
            GeneratedDeclarationSlot::new(
                directory.path(),
                "safe",
                language("decl"),
                "# marker\n",
                1,
                "../tmp-",
            ),
            Err(GeneratedDeclarationSlotError::InvalidTemporaryPrefix { .. })
        ));
    }
}
