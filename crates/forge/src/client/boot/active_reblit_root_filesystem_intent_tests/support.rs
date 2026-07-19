use std::{
    ffi::CString,
    fs, io,
    os::unix::{ffi::OsStrExt as _, fs::PermissionsExt as _},
    path::{Path, PathBuf},
};

use tempfile::TempDir;

use super::super::{
    ActiveReblitRootFilesystemIntentError, PreparedActiveReblitRootFilesystemIntent, root_filesystem_intent_path,
};
use crate::Installation;

pub(super) const ROOT_LOCATOR: &str = "PARTUUID=11111111-2222-3333-4444-555555555555";

pub(super) struct Fixture {
    _temporary: TempDir,
    pub(super) root: PathBuf,
    pub(super) installation: Installation,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let installation = Installation::open(&root, None).unwrap();
        let source_directory = root.join("etc/cast");
        fs::create_dir_all(&source_directory).unwrap();
        for directory in [root.join("etc"), source_directory] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            _temporary: temporary,
            root,
            installation,
        }
    }

    pub(super) fn source_path(&self) -> PathBuf {
        root_filesystem_intent_path(&self.installation)
    }

    pub(super) fn write_source(&self, source: impl AsRef<[u8]>) {
        fs::write(self.source_path(), source).unwrap();
        fs::set_permissions(self.source_path(), fs::Permissions::from_mode(0o644)).unwrap();
    }

    pub(super) fn write_root(&self, root: &str) {
        self.write_source(authored_root(root));
    }

    pub(super) fn prepare(
        &self,
    ) -> Result<PreparedActiveReblitRootFilesystemIntent, ActiveReblitRootFilesystemIntentError> {
        PreparedActiveReblitRootFilesystemIntent::prepare(&self.installation)
    }
}

pub(super) fn authored_root(root: &str) -> String {
    format!("let cast = import! cast.root_filesystem.v1\ncast.root_filesystem {{ root = {root:?} }}\n")
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct TreeSnapshot(Vec<TreeEntry>);

#[derive(Debug, Eq, PartialEq)]
struct TreeEntry {
    relative: PathBuf,
    mode: u32,
    links: u64,
    bytes: Vec<u8>,
}

impl TreeSnapshot {
    pub(super) fn capture(root: &Path) -> Self {
        let mut entries = Vec::new();
        capture_directory(root, root, &mut entries);
        Self(entries)
    }
}

fn capture_directory(root: &Path, directory: &Path, entries: &mut Vec<TreeEntry>) {
    let mut children = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort();
    for path in children {
        let metadata = fs::symlink_metadata(&path).unwrap();
        let bytes = if metadata.file_type().is_file() {
            fs::read(&path).unwrap()
        } else if metadata.file_type().is_symlink() {
            fs::read_link(&path).unwrap().as_os_str().as_bytes().to_vec()
        } else {
            Vec::new()
        };
        entries.push(TreeEntry {
            relative: path.strip_prefix(root).unwrap().to_owned(),
            mode: metadata.permissions().mode(),
            links: std::os::unix::fs::MetadataExt::nlink(&metadata),
            bytes,
        });
        if metadata.file_type().is_dir() {
            capture_directory(root, &path, entries);
        }
    }
}

pub(super) fn set_test_xattr(path: &Path) -> io::Result<bool> {
    let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
    let value = b"rejected";
    // SAFETY: both C strings and the complete value remain live for the call.
    let result = unsafe {
        nix::libc::setxattr(
            encoded.as_ptr(),
            c"user.cast-root-filesystem-test".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    classify_optional_metadata_mutation(result)
}

pub(super) fn set_access_acl(path: &Path) -> io::Result<bool> {
    let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    let named = unsafe { nix::libc::geteuid() }.wrapping_add(1);
    for (tag, permissions, id) in [
        (0x01_u16, 0x07_u16, u32::MAX),
        (0x02, 0x04, named),
        (0x04, 0x05, u32::MAX),
        (0x10, 0x05, u32::MAX),
        (0x20, 0x05, u32::MAX),
    ] {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&permissions.to_le_bytes());
        bytes.extend_from_slice(&id.to_le_bytes());
    }
    // SAFETY: both C strings and the complete ACL bytes remain live.
    let result = unsafe {
        nix::libc::setxattr(
            encoded.as_ptr(),
            c"system.posix_acl_access".as_ptr(),
            bytes.as_ptr().cast(),
            bytes.len(),
            0,
        )
    };
    classify_optional_metadata_mutation(result)
}

fn classify_optional_metadata_mutation(result: i32) -> io::Result<bool> {
    if result == 0 {
        Ok(true)
    } else {
        let source = io::Error::last_os_error();
        if matches!(
            source.raw_os_error(),
            Some(nix::libc::EOPNOTSUPP) | Some(nix::libc::EPERM) | Some(nix::libc::EACCES) | Some(nix::libc::EINVAL)
        ) {
            Ok(false)
        } else {
            Err(source)
        }
    }
}
