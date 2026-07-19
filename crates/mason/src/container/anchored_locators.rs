use std::{
    ffi::CString,
    io,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd, FromRawFd as _, RawFd},
        unix::ffi::OsStrExt as _,
    },
    path::Path,
};

use ::container::AnchoredLocator;

use super::{Error, FrozenSandbox, PinnedFrozenMountSource, workspace_relative};

impl FrozenSandbox {
    /// Build the root locator below the exact workspace retained by this
    /// sandbox. The Forge guard remains the leaf identity authority.
    pub(super) fn root_locator(&self, root_path: &Path, root_anchor: &impl AsRawFd) -> Result<AnchoredLocator, Error> {
        let relative = workspace_relative(&self.workspace.path, root_path)?;
        AnchoredLocator::beneath(&self.workspace.path, &self.workspace.path_anchor, relative, root_anchor)
            .map_err(Error::FrozenRootLocator)
    }

    /// Build one external source locator below the same workspace identity.
    /// The ordinary source handle is retained separately for revalidation and
    /// publication; only its parallel O_PATH handle enters this locator.
    pub(super) fn pinned_source_locator(
        &self,
        source: &PinnedFrozenMountSource,
        display: &Path,
    ) -> Result<AnchoredLocator, Error> {
        AnchoredLocator::beneath(
            &self.workspace.path,
            &self.workspace.path_anchor,
            &source.relative,
            &source.path_anchor,
        )
        .map_err(|source| Error::FrozenBindSourceLocator {
            path: display.to_owned(),
            source,
        })
    }
}

/// Open one directory as O_PATH below an already-authenticated workspace.
///
/// This is deliberately descriptor-relative and forbids mount crossings,
/// links, and escape. It never canonicalizes or reopens through an ambient
/// absolute pathname.
pub(super) fn open_workspace_path_anchor(base: &impl AsRawFd, relative: &Path) -> io::Result<std::fs::File> {
    let encoded = CString::new(relative.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "workspace-relative path contains NUL"))?;
    // SAFETY: an all-zero open_how is valid before setting its public fields.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from((nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC) as u32);
    how.resolve = nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_XDEV
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS;
    // SAFETY: the base descriptor, encoded path, and open_how remain live for
    // the syscall. Success returns one fresh descriptor.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            base.as_raw_fd(),
            encoded.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = RawFd::try_from(result)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {result}")))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn workspace_path_anchor_is_opath_and_rejects_links() {
        let temporary = crate::private_tempdir();
        let child = temporary.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let base = crate::paths::pin_workspace_root(temporary.path()).unwrap();
        let anchor = open_workspace_path_anchor(&base, Path::new("child")).unwrap();

        // SAFETY: F_GETFL only reads flags from a live descriptor.
        let flags = unsafe { nix::libc::fcntl(anchor.as_raw_fd(), nix::libc::F_GETFL) };
        assert_ne!(flags, -1);
        assert_eq!(flags & nix::libc::O_PATH, nix::libc::O_PATH);

        symlink(&child, temporary.path().join("redirect")).unwrap();
        let error = open_workspace_path_anchor(&base, Path::new("redirect")).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(nix::libc::ELOOP));
    }
}
