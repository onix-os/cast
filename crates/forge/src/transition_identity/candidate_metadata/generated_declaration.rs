//! Neutral generated-declaration payload for paired candidate metadata.
//!
//! This binds canonical bytes to one exact active language/ownership authority
//! while retaining the complete registered authority set. Public filenames
//! are derived from opaque language profiles, so stale alternate-language
//! names can be rejected by the descriptor-retaining pair transaction.

use std::{
    ffi::{CStr, CString},
    path::{Path, PathBuf},
};

use config::declaration::{
    GeneratedDeclarationAuthority,
    RegisteredGeneratedDeclarationAuthorities,
};

use super::{CandidateMetadataError, bounded_output};

const SYSTEM_MODEL_LOGICAL_NAME: &str = "system-model";

/// One authority-bound generated output within the metadata pair.
#[derive(Debug)]
pub(super) struct GeneratedDeclarationOutput {
    authorities: RegisteredGeneratedDeclarationAuthorities,
    active_file_name: CString,
    alternate_file_names: Vec<CString>,
    bytes: Vec<u8>,
}

impl GeneratedDeclarationOutput {
    pub(super) fn system_model(
        authorities: RegisteredGeneratedDeclarationAuthorities,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<Self, CandidateMetadataError> {
        let active_file_name = system_model_file_name(
            authorities.active_authority(),
        )?;
        let alternate_file_names = authorities
            .alternate_authorities()
            .map(system_model_file_name)
            .collect::<Result<Vec<_>, _>>()?;
        let bytes = bytes.into();
        bounded_output(
            active_file_name.to_string_lossy().into_owned(),
            &bytes,
        )?;
        let output = Self {
            authorities,
            active_file_name,
            alternate_file_names,
            bytes,
        };
        output.revalidate_authority()?;
        Ok(output)
    }

    /// Reassert that the retained output still matches its language, marker,
    /// complete registration set, and fixed logical slot before every
    /// publication/proof operation.
    pub(super) fn revalidate_authority(&self) -> Result<(), CandidateMetadataError> {
        let public_name = system_model_public_name(
            self.authorities.active_authority(),
        );
        if self.active_file_name.to_bytes() != public_name.as_bytes() {
            return Err(CandidateMetadataError::InvalidGeneratedDeclarationName {
                name: public_name,
            });
        }
        let expected_alternates = self
            .authorities
            .alternate_authorities()
            .map(system_model_public_name)
            .collect::<Vec<_>>();
        if self.alternate_file_names.len() != expected_alternates.len()
            || self
                .alternate_file_names
                .iter()
                .zip(&expected_alternates)
                .any(|(retained, expected)| {
                    retained.to_bytes() != expected.as_bytes()
                })
        {
            return Err(CandidateMetadataError::InvalidGeneratedDeclarationName {
                name: expected_alternates.join(", "),
            });
        }
        if !self
            .bytes
            .starts_with(self.authorities.active_authority().ownership_marker())
        {
            return Err(CandidateMetadataError::MissingGeneratedDeclarationMarker {
                name: public_name,
            });
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn authority(&self) -> &GeneratedDeclarationAuthority {
        self.authorities.active_authority()
    }

    #[cfg(test)]
    pub(super) fn authorities(
        &self,
    ) -> &RegisteredGeneratedDeclarationAuthorities {
        &self.authorities
    }

    pub(super) fn file_name(&self) -> &CStr {
        &self.active_file_name
    }

    pub(super) fn alternate_file_names(
        &self,
    ) -> impl Iterator<Item = &CStr> {
        self.alternate_file_names.iter().map(CString::as_c_str)
    }

    pub(super) fn path_in(&self, directory: &Path) -> PathBuf {
        directory.join(self.active_file_name.to_string_lossy().as_ref())
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn system_model_public_name(
    authority: &GeneratedDeclarationAuthority,
) -> String {
    format!(
        "{SYSTEM_MODEL_LOGICAL_NAME}.{}",
        authority.language_spec().extension(),
    )
}

fn system_model_file_name(
    authority: &GeneratedDeclarationAuthority,
) -> Result<CString, CandidateMetadataError> {
    let public_name = system_model_public_name(authority);
    CString::new(public_name.clone()).map_err(|_| {
        CandidateMetadataError::InvalidGeneratedDeclarationName {
            name: public_name,
        }
    })
}
