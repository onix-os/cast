//! Mason composition for exact generated declaration slots.

use std::{ffi::OsStr, io, path::Path};

use config::declaration::{
    GeneratedDeclarationSlot, SaveDeclarationError,
};
use declarative_config::LanguageSpec;

/// Construct the one generated declaration authority for an exact public
/// filename. Domain codecs supply their own ownership marker; the language
/// descriptor supplies only the extension and engine identity.
pub(crate) fn declaration_slot(
    path: &Path,
    expected_file_name: &str,
    language: &LanguageSpec,
    ownership_marker: &str,
    max_generated_bytes: usize,
) -> io::Result<GeneratedDeclarationSlot> {
    if path.file_name() != Some(OsStr::new(expected_file_name)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "generated declaration path must end in {expected_file_name:?}"
            ),
        ));
    }
    let extension = format!(".{}", language.extension());
    let basename = expected_file_name
        .strip_suffix(&extension)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "generated declaration filename does not match its language extension",
            )
        })?;
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    GeneratedDeclarationSlot::new(
        directory,
        basename,
        language.clone(),
        ownership_marker,
        max_generated_bytes,
        format!(".{expected_file_name}.tmp-"),
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

pub(crate) fn save_error_into_io(error: SaveDeclarationError) -> io::Error {
    let kind = match &error {
        SaveDeclarationError::CreateDirectory { source, .. }
        | SaveDeclarationError::ReadExisting { source, .. }
        | SaveDeclarationError::CreateTemporary { source, .. }
        | SaveDeclarationError::WriteTemporary { source, .. }
        | SaveDeclarationError::SyncTemporary { source, .. }
        | SaveDeclarationError::CleanupTemporary { source, .. }
        | SaveDeclarationError::Rename { source, .. }
        | SaveDeclarationError::SyncDirectory { source, .. } => source.kind(),
        SaveDeclarationError::AuthoredDeclaration { .. } => {
            io::ErrorKind::PermissionDenied
        }
        SaveDeclarationError::MissingOwnershipMarker { .. }
        | SaveDeclarationError::GeneratedTooLarge { .. } => {
            io::ErrorKind::InvalidData
        }
    };
    io::Error::new(kind, error)
}
