use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::{Component, Path, PathBuf},
};

use declarative_config::LanguageSpec;
use fs_err as fs;

use super::{
    atomic_persistence::{
        AtomicAuthoritySwitch, AtomicWrite, AuthoritySwitchOrigin,
        AuthoritySwitchPhase, ExpectedTarget,
        atomic_authority_switch_with_hook,
        require_same_generated_declaration,
        require_same_generated_declaration_markers,
    },
    managed_directory::{
        FileSnapshot, ManagedDirectory, inspect_existing_declaration,
        inspect_existing_declaration_markers,
    },
    storage_error::{
        DeleteDeclarationError, GeneratedDeclarationSlotError,
        SaveDeclarationError,
    },
};

const MAX_FILE_NAME_BYTES: usize = 255;
const TEMPORARY_RANDOM_HEX_BYTES: usize = 32;
const SWITCH_RESIDUE_SUFFIX: &str = ".generated-switch";

/// One language's right to own a generated logical declaration.
///
/// The language descriptor selects the public extension and adapter identity.
/// The marker is domain-specific: it proves that this logical slot, rather
/// than merely the language runtime, generated the existing bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedDeclarationAuthority {
    language: LanguageSpec,
    ownership_marker: Vec<u8>,
}

impl GeneratedDeclarationAuthority {
    pub fn new(
        language: LanguageSpec,
        ownership_marker: impl Into<Vec<u8>>,
    ) -> Result<Self, GeneratedDeclarationSlotError> {
        let ownership_marker = ownership_marker.into();
        if ownership_marker.is_empty() {
            return Err(GeneratedDeclarationSlotError::InvalidOwnershipMarker);
        }
        Ok(Self {
            language,
            ownership_marker,
        })
    }

    pub fn language_spec(&self) -> &LanguageSpec {
        &self.language
    }

    pub fn ownership_marker(&self) -> &[u8] {
        &self.ownership_marker
    }
}

/// The complete registered authority policy for one generated declaration.
///
/// Registration is keyed by public extension. The active authority must be
/// present as the exact same descriptor, not merely use a registered
/// extension. Keeping the complete set alongside the active descriptor lets
/// every consumer reject stale or conflicting alternate public names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredGeneratedDeclarationAuthorities {
    authorities: BTreeMap<String, GeneratedDeclarationAuthority>,
    active_extension: String,
}

impl RegisteredGeneratedDeclarationAuthorities {
    pub fn new(
        registered_authorities: impl IntoIterator<
            Item = GeneratedDeclarationAuthority,
        >,
        active_authority: GeneratedDeclarationAuthority,
    ) -> Result<Self, GeneratedDeclarationSlotError> {
        let mut authorities = BTreeMap::new();
        for authority in registered_authorities {
            let extension = authority.language_spec().extension().to_owned();
            if authorities.insert(extension.clone(), authority).is_some() {
                return Err(
                    GeneratedDeclarationSlotError::DuplicateAuthorityExtension {
                        extension,
                    },
                );
            }
        }
        if authorities.is_empty() {
            return Err(GeneratedDeclarationSlotError::NoRegisteredAuthorities);
        }
        let active_extension = active_authority
            .language_spec()
            .extension()
            .to_owned();
        match authorities.get(&active_extension) {
            None => {
                return Err(
                    GeneratedDeclarationSlotError::ActiveAuthorityNotRegistered {
                        extension: active_extension,
                    },
                );
            }
            Some(registered) if registered != &active_authority => {
                return Err(
                    GeneratedDeclarationSlotError::ActiveAuthorityMismatch {
                        extension: active_extension,
                    },
                );
            }
            Some(_) => {}
        }
        Ok(Self {
            authorities,
            active_extension,
        })
    }

    pub fn active_authority(&self) -> &GeneratedDeclarationAuthority {
        self.authorities
            .get(&self.active_extension)
            .expect("active generated declaration authority was validated")
    }

    pub fn registered_authorities(
        &self,
    ) -> impl ExactSizeIterator<Item = &GeneratedDeclarationAuthority> {
        self.authorities.values()
    }

    pub fn alternate_authorities(
        &self,
    ) -> impl Iterator<Item = &GeneratedDeclarationAuthority> {
        self.authorities.iter().filter_map(|(extension, authority)| {
            (extension != &self.active_extension).then_some(authority)
        })
    }
}

/// Exclusive authority for one generated declaration path.
///
/// Language selection and ownership are explicit. The marker is used only to
/// distinguish generated content from authored content; it never detects a
/// language or selects an evaluator. Markers are a cooperative ownership
/// assertion, not cryptographic authentication; restart recovery applies the
/// exact registered domain marker to the hidden switch residue as well as to
/// each public extension.
#[derive(Debug, Clone)]
pub struct GeneratedDeclarationSlot {
    directory: PathBuf,
    name: String,
    authorities: RegisteredGeneratedDeclarationAuthorities,
    size_limit: usize,
    temporary_prefix: String,
    switch_residue_name: OsString,
}

impl GeneratedDeclarationSlot {
    pub fn with_registered_authorities(
        directory: impl Into<PathBuf>,
        name: impl Into<String>,
        registered_authorities: impl IntoIterator<
            Item = GeneratedDeclarationAuthority,
        >,
        active_authority: GeneratedDeclarationAuthority,
        size_limit: usize,
        temporary_prefix: impl Into<String>,
    ) -> Result<Self, GeneratedDeclarationSlotError> {
        let name = name.into();
        if !is_safe_component(&name) {
            return Err(GeneratedDeclarationSlotError::InvalidName { name });
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
        let registered_authorities =
            registered_authorities.into_iter().collect::<Vec<_>>();
        for authority in &registered_authorities {
            let extension = authority.language_spec().extension();
            let file_name_bytes = name
                .len()
                .saturating_add(1)
                .saturating_add(extension.len());
            if file_name_bytes > MAX_FILE_NAME_BYTES {
                return Err(GeneratedDeclarationSlotError::InvalidName {
                    name,
                });
            }
            if authority.ownership_marker().len() > size_limit {
                return Err(
                    GeneratedDeclarationSlotError::OwnershipMarkerTooLarge {
                        extension: extension.to_owned(),
                        size: authority.ownership_marker().len(),
                        limit: size_limit,
                    },
                );
            }
        }
        let authorities = RegisteredGeneratedDeclarationAuthorities::new(
            registered_authorities,
            active_authority,
        )?;
        let switch_residue_name = OsString::from(format!(
            ".{name}{SWITCH_RESIDUE_SUFFIX}",
        ));
        if switch_residue_name.len() > MAX_FILE_NAME_BYTES {
            return Err(GeneratedDeclarationSlotError::InvalidName { name });
        }
        Ok(Self {
            directory: directory.into(),
            name,
            authorities,
            size_limit,
            temporary_prefix,
            switch_residue_name,
        })
    }

    pub fn language_spec(&self) -> &LanguageSpec {
        self.active_authority().language_spec()
    }

    pub fn path(&self) -> PathBuf {
        self.directory.join(self.file_name())
    }

    pub fn ownership_marker(&self) -> &[u8] {
        self.active_authority().ownership_marker()
    }

    pub fn save(&self, bytes: &[u8]) -> Result<PathBuf, SaveDeclarationError> {
        self.save_with_phase_hook(bytes, |_| Ok(()))
    }

    #[cfg(test)]
    pub(crate) fn save_with_hook(
        &self,
        bytes: &[u8],
        before_commit: impl FnOnce(&Path),
    ) -> Result<PathBuf, SaveDeclarationError> {
        let mut before_commit = Some(before_commit);
        self.save_internal(bytes, |phase, hook_path| {
            if phase == AuthoritySwitchPhase::BeforeCommit {
                if let Some(before_commit) = before_commit.take() {
                    before_commit(hook_path);
                }
            }
            Ok(())
        })
    }

    fn save_with_phase_hook(
        &self,
        bytes: &[u8],
        mut phase_hook: impl FnMut(AuthoritySwitchPhase) -> io::Result<()>,
    ) -> Result<PathBuf, SaveDeclarationError> {
        self.save_internal(bytes, |phase, _| phase_hook(phase))
    }

    fn save_internal(
        &self,
        bytes: &[u8],
        mut phase_hook: impl FnMut(
            AuthoritySwitchPhase,
            &Path,
        ) -> io::Result<()>,
    ) -> Result<PathBuf, SaveDeclarationError> {
        let path = self.path();
        if !bytes.starts_with(self.ownership_marker()) {
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
        let mut public = self
            .inspect_public_candidates(&directory)
            .map_err(|failure| SaveDeclarationError::ReadExisting {
                path: failure.path,
                source: failure.source,
            })?;
        if public.len() > 1 {
            return Err(SaveDeclarationError::ReadExisting {
                path: self.logical_path(),
                source: collision_error(&public),
            });
        }
        let candidate = public.pop();
        let mut residue = self
            .inspect_residue(&directory)
            .map_err(|failure| SaveDeclarationError::ReadExisting {
                path: failure.path,
                source: failure.source,
            })?;
        if let Some(candidate) = &candidate {
            if !candidate.generated {
                return Err(SaveDeclarationError::AuthoredDeclaration {
                    path: candidate.path.clone(),
                });
            }
        }
        if let Some(existing_residue) = residue {
            if !existing_residue.generated {
                return Err(SaveDeclarationError::AuthoredDeclaration {
                    path: self.switch_residue_path(),
                });
            }
            if candidate.is_some() {
                self.cleanup_residue_for_save(&directory, existing_residue)?;
                residue = None;
            }
        }

        let public_file_names = self.public_file_names();
        match (candidate, residue) {
            (Some(candidate), None) if candidate.active => {
                let absent_file_names = public_file_names
                    .iter()
                    .filter(|name| **name != candidate.file_name)
                    .cloned()
                    .collect::<Vec<_>>();
                super::atomic_persistence::atomic_write_with_hook(
                    AtomicWrite {
                        directory: &directory,
                        file_name: &candidate.file_name,
                        path: &candidate.path,
                        bytes,
                        expected: ExpectedTarget::Generated(candidate.identity),
                        size_limit: self.size_limit,
                        ownership_marker: self.ownership_marker(),
                        temporary_prefix: &self.temporary_prefix,
                        absent_file_names: &absent_file_names,
                    },
                    |temporary_path| {
                        phase_hook(
                            AuthoritySwitchPhase::BeforeCommit,
                            temporary_path,
                        )
                    },
                )?;
            }
            (Some(candidate), None) => {
                let markers = self.registered_markers();
                let residue_path = self.switch_residue_path();
                atomic_authority_switch_with_hook(
                    AtomicAuthoritySwitch {
                        directory: &directory,
                        active_file_name: &self.file_name(),
                        active_path: &path,
                        bytes,
                        active_ownership_marker: self.ownership_marker(),
                        origin: AuthoritySwitchOrigin::Public {
                            file_name: &candidate.file_name,
                            path: &candidate.path,
                            identity: candidate.identity,
                            ownership_marker: &candidate.ownership_marker,
                        },
                        residue_name: &self.switch_residue_name,
                        residue_path: &residue_path,
                        public_file_names: &public_file_names,
                        size_limit: self.size_limit,
                        registered_markers: &markers,
                        temporary_prefix: &self.temporary_prefix,
                    },
                    |phase| phase_hook(phase, &path),
                )?;
            }
            (None, Some(residue)) => {
                let markers = self.registered_markers();
                let residue_path = self.switch_residue_path();
                atomic_authority_switch_with_hook(
                    AtomicAuthoritySwitch {
                        directory: &directory,
                        active_file_name: &self.file_name(),
                        active_path: &path,
                        bytes,
                        active_ownership_marker: self.ownership_marker(),
                        origin: AuthoritySwitchOrigin::Retired {
                            identity: residue.identity,
                        },
                        residue_name: &self.switch_residue_name,
                        residue_path: &residue_path,
                        public_file_names: &public_file_names,
                        size_limit: self.size_limit,
                        registered_markers: &markers,
                        temporary_prefix: &self.temporary_prefix,
                    },
                    |phase| phase_hook(phase, &path),
                )?;
            }
            (None, None) => {
                let file_name = self.file_name();
                let absent_file_names = public_file_names
                    .iter()
                    .filter(|name| **name != file_name)
                    .cloned()
                    .collect::<Vec<_>>();
                super::atomic_persistence::atomic_write_with_hook(
                    AtomicWrite {
                        directory: &directory,
                        file_name: &file_name,
                        path: &path,
                        bytes,
                        expected: ExpectedTarget::Missing,
                        size_limit: self.size_limit,
                        ownership_marker: self.ownership_marker(),
                        temporary_prefix: &self.temporary_prefix,
                        absent_file_names: &absent_file_names,
                    },
                    |temporary_path| {
                        phase_hook(
                            AuthoritySwitchPhase::BeforeCommit,
                            temporary_path,
                        )
                    },
                )?;
            }
            (Some(_), Some(_)) => {
                unreachable!("a public candidate causes residue recovery first")
            }
        }
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
        let mut public = self
            .inspect_public_candidates(&directory)
            .map_err(|failure| DeleteDeclarationError::ReadExisting {
                path: failure.path,
                source: failure.source,
            })?;
        if public.len() > 1 {
            return Err(DeleteDeclarationError::ReadExisting {
                path: self.logical_path(),
                source: collision_error(&public),
            });
        }
        let candidate = public.pop();
        let residue = self
            .inspect_residue(&directory)
            .map_err(|failure| DeleteDeclarationError::ReadExisting {
                path: failure.path,
                source: failure.source,
            })?;
        if candidate.is_none() && residue.is_none() {
            return Ok(());
        }
        if let Some(candidate) = &candidate {
            if !candidate.generated {
                return Err(DeleteDeclarationError::AuthoredDeclaration {
                    path: candidate.path.clone(),
                });
            }
        }
        if let Some(residue) = residue {
            if !residue.generated {
                return Err(DeleteDeclarationError::AuthoredDeclaration {
                    path: self.switch_residue_path(),
                });
            }
        }
        before_remove();
        directory.verify_path().map_err(|source| {
            DeleteDeclarationError::ReadExisting {
                path: path.clone(),
                source,
            }
        })?;
        if let Some(residue) = residue {
            let markers = self.registered_markers();
            let residue_path = self.switch_residue_path();
            require_same_generated_declaration_markers(
                &directory,
                &self.switch_residue_name,
                residue.identity,
                self.size_limit,
                &markers,
            )
            .map_err(|source| DeleteDeclarationError::ReadExisting {
                path: residue_path.clone(),
                source,
            })?;
            directory.unlink(&self.switch_residue_name).map_err(|source| {
                DeleteDeclarationError::Remove {
                    path: residue_path,
                    source,
                }
            })?;
        }
        if let Some(candidate) = candidate {
            require_same_generated_declaration(
                &directory,
                &candidate.file_name,
                candidate.identity,
                self.size_limit,
                &candidate.ownership_marker,
            )
            .map_err(|source| DeleteDeclarationError::ReadExisting {
                path: candidate.path.clone(),
                source,
            })?;
            directory.unlink(&candidate.file_name).map_err(|source| {
                DeleteDeclarationError::Remove {
                    path: candidate.path,
                    source,
                }
            })?;
        }
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
        self.file_name_for(self.language_spec().extension())
    }

    fn file_name_for(&self, extension: &str) -> OsString {
        OsString::from(format!("{}.{}", self.name, extension))
    }

    fn public_file_names(&self) -> Vec<OsString> {
        self.authorities
            .registered_authorities()
            .map(|authority| {
                self.file_name_for(authority.language_spec().extension())
            })
            .collect()
    }

    fn logical_path(&self) -> PathBuf {
        self.directory.join(&self.name)
    }

    fn switch_residue_path(&self) -> PathBuf {
        self.directory.join(&self.switch_residue_name)
    }

    fn registered_markers(&self) -> Vec<&[u8]> {
        self.authorities
            .registered_authorities()
            .map(GeneratedDeclarationAuthority::ownership_marker)
            .collect()
    }

    fn inspect_public_candidates(
        &self,
        directory: &ManagedDirectory,
    ) -> Result<Vec<ExistingAuthority>, InspectionFailure> {
        let mut candidates = Vec::new();
        for authority in self.authorities.registered_authorities() {
            let extension = authority.language_spec().extension();
            let file_name = self.file_name_for(extension);
            let path = self.directory.join(&file_name);
            let existing = inspect_existing_declaration(
                directory,
                &file_name,
                self.size_limit,
                authority.ownership_marker(),
            )
            .map_err(|source| InspectionFailure {
                path: path.clone(),
                source,
            })?;
            if let Some(existing) = existing {
                candidates.push(ExistingAuthority {
                    file_name,
                    path,
                    identity: existing.identity(),
                    generated: existing.is_generated(),
                    active: authority == self.active_authority(),
                    ownership_marker: authority.ownership_marker().to_vec(),
                });
            }
        }
        Ok(candidates)
    }

    fn inspect_residue(
        &self,
        directory: &ManagedDirectory,
    ) -> Result<Option<ExistingResidue>, InspectionFailure> {
        let path = self.switch_residue_path();
        let markers = self.registered_markers();
        inspect_existing_declaration_markers(
            directory,
            &self.switch_residue_name,
            self.size_limit,
            &markers,
        )
        .map(|existing| {
            existing.map(|existing| ExistingResidue {
                identity: existing.identity(),
                generated: existing.is_generated(),
            })
        })
        .map_err(|source| InspectionFailure { path, source })
    }

    fn cleanup_residue_for_save(
        &self,
        directory: &ManagedDirectory,
        residue: ExistingResidue,
    ) -> Result<(), SaveDeclarationError> {
        let path = self.switch_residue_path();
        let markers = self.registered_markers();
        require_same_generated_declaration_markers(
            directory,
            &self.switch_residue_name,
            residue.identity,
            self.size_limit,
            &markers,
        )
        .map_err(|source| SaveDeclarationError::ReadExisting {
            path: path.clone(),
            source,
        })?;
        directory.unlink(&self.switch_residue_name).map_err(|source| {
            SaveDeclarationError::CleanupTemporary {
                path: path.clone(),
                source,
            }
        })?;
        directory.sync().map_err(|source| {
            SaveDeclarationError::SyncDirectory {
                path: self.directory.clone(),
                source,
            }
        })?;
        directory.verify_path().map_err(|source| {
            SaveDeclarationError::SyncDirectory {
                path: self.directory.clone(),
                source,
            }
        })
    }

    fn active_authority(&self) -> &GeneratedDeclarationAuthority {
        self.authorities.active_authority()
    }
}

#[derive(Debug)]
struct ExistingAuthority {
    file_name: OsString,
    path: PathBuf,
    identity: FileSnapshot,
    generated: bool,
    active: bool,
    ownership_marker: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct ExistingResidue {
    identity: FileSnapshot,
    generated: bool,
}

#[derive(Debug)]
struct InspectionFailure {
    path: PathBuf,
    source: io::Error,
}

fn collision_error(candidates: &[ExistingAuthority]) -> io::Error {
    let paths = candidates
        .iter()
        .map(|candidate| candidate.path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("multiple registered generated declaration candidates: {paths}"),
    )
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
mod tests;
