use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

const PRIVATE_INSTALLATION_ROOT_MODE: u32 = 0o700;

/// Prepare a root created by test scaffolding for the production installation
/// root policy. `create_dir` intentionally honors the ambient umask, so its
/// usual 0777 request becomes group-writable under a 0002 developer umask.
pub(crate) fn prepare_private_installation_root(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_INSTALLATION_ROOT_MODE)).unwrap();
}

/// Create a temporary directory suitable for use as an installation root.
pub(crate) fn private_installation_tempdir() -> tempfile::TempDir {
    let temporary = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(PRIVATE_INSTALLATION_ROOT_MODE))
        .tempdir()
        .unwrap();
    prepare_private_installation_root(temporary.path());
    temporary
}
