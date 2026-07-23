//! Container locators derived from already-retained directory authority.

use std::{
    ffi::CString,
    io,
    os::{
        fd::AsRawFd as _,
        unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    },
    path::Path,
};

use container::AnchoredLocator;

use crate::Installation;

pub(super) fn exact_directory(path: &Path, retained: &std::fs::File) -> io::Result<AnchoredLocator> {
    let witness = open_path_directory(retained, Path::new("."))?;
    require_same_directory(retained, &witness)?;
    AnchoredLocator::exact(path, &witness).map_err(locator_error)
}

pub(super) fn beneath_directory(
    absolute_base_path: &Path,
    retained_base: &std::fs::File,
    relative_path: &Path,
    retained_leaf: &std::fs::File,
) -> io::Result<AnchoredLocator> {
    let base_witness = open_path_directory(retained_base, Path::new("."))?;
    require_same_directory(retained_base, &base_witness)?;
    let leaf_witness = open_path_directory(retained_base, relative_path)?;
    require_same_directory(retained_leaf, &leaf_witness)?;
    AnchoredLocator::beneath(absolute_base_path, &base_witness, relative_path, &leaf_witness).map_err(locator_error)
}

pub(super) fn beneath_installation_directory(
    installation: &Installation,
    path: &Path,
    retained: &std::fs::File,
) -> io::Result<AnchoredLocator> {
    let relative = installation_relative_path(installation, path)?;
    beneath_directory(&installation.root, installation.root_directory(), relative, retained)
}

fn installation_relative_path<'path>(installation: &Installation, path: &'path Path) -> io::Result<&'path Path> {
    path.strip_prefix(&installation.root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "anchored directory is outside the authenticated installation root",
        )
    })
}

fn open_path_directory(parent: &std::fs::File, relative: &Path) -> io::Result<std::fs::File> {
    let relative = CString::new(relative.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "anchored directory path contains NUL"))?;
    crate::linux_fs::openat2_file(
        parent.as_raw_fd(),
        &relative,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
        0,
        crate::linux_fs::controlled_resolution(),
    )
}

fn require_same_directory(expected: &std::fs::File, actual: &std::fs::File) -> io::Result<()> {
    let expected = expected.metadata()?;
    let actual = actual.metadata()?;
    if expected.file_type().is_dir()
        && actual.file_type().is_dir()
        && expected.dev() == actual.dev()
        && expected.ino() == actual.ino()
    {
        Ok(())
    } else {
        Err(io::Error::other(
            "retained directory and its O_PATH locator witness differ",
        ))
    }
}

fn locator_error(error: container::AnchoredLocatorError) -> io::Error {
    io::Error::other(error)
}
