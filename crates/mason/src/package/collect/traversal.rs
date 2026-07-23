use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    },
    path::{Path, PathBuf},
    sync::Arc,
};

use nix::libc;

use super::{
    Error,
    filesystem::{changed, metadata, openat2_file},
    inventory::{DirectoryId, WitnessGraph},
};

#[derive(Debug)]
pub(super) enum Task {
    Scan {
        directory: Arc<DirectoryHandle>,
        include_directory: bool,
        depth: usize,
    },
    Visit {
        parent: Arc<DirectoryHandle>,
        name: OsString,
        relative: PathBuf,
        depth: usize,
    },
    Finalize {
        directory: Arc<DirectoryHandle>,
        include_directory: bool,
        output_start: usize,
    },
}

impl Task {
    pub(super) fn path(&self) -> &Path {
        match self {
            Task::Scan { directory, .. } | Task::Finalize { directory, .. } => &directory.display_path,
            Task::Visit { parent, .. } => &parent.display_path,
        }
    }
}

#[derive(Debug)]
pub(super) enum InventoryTask {
    Scan {
        directory: Arc<DirectoryHandle>,
        depth: usize,
    },
    Visit {
        parent: Arc<DirectoryHandle>,
        name: OsString,
        relative: PathBuf,
        depth: usize,
    },
    Finalize {
        directory: Arc<DirectoryHandle>,
    },
}

impl InventoryTask {
    pub(super) fn path(&self) -> &Path {
        match self {
            Self::Scan { directory, .. } | Self::Finalize { directory } => &directory.display_path,
            Self::Visit { parent, .. } => &parent.display_path,
        }
    }
}

#[derive(Debug)]
pub(super) struct DirectoryHandle {
    pub(super) file: File,
    pub(super) relative: PathBuf,
    pub(super) display_path: PathBuf,
    pub(super) snapshot: FileSnapshot,
    pub(super) anchor: Arc<RootAnchor>,
    pub(super) witness: Arc<WitnessGraph>,
    pub(super) witness_id: DirectoryId,
}

#[derive(Debug)]
pub(super) struct RootAnchor {
    pub(super) file: File,
    pub(super) path: PathBuf,
    pub(super) identity: NodeIdentity,
}

impl RootAnchor {
    pub(super) fn open(path: &Path) -> Result<Self, Error> {
        let file = openat2_file(
            libc::AT_FDCWD,
            path.as_os_str().as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            path,
        )?;
        let metadata = metadata(&file, "stat package root", path)?;
        if !metadata.file_type().is_dir() {
            return Err(Error::UnsupportedFileType {
                path: path.to_owned(),
                kind: "non-directory root",
            });
        }
        Ok(Self {
            file,
            path: path.to_owned(),
            identity: NodeIdentity::from_metadata(&metadata),
        })
    }

    pub(super) fn verify_path_node(&self) -> Result<(), Error> {
        let reopened = openat2_file(
            libc::AT_FDCWD,
            self.path.as_os_str().as_bytes(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            &self.path,
        )?;
        let current = NodeIdentity::from_metadata(&metadata(&reopened, "verify package root", &self.path)?);
        if current == self.identity {
            Ok(())
        } else {
            Err(changed(&self.path, "package root was replaced"))
        }
    }

    pub(super) fn open_directory(&self, relative: &Path) -> Result<File, Error> {
        self.verify_path_node()?;
        let path = if relative.as_os_str().is_empty() {
            OsStr::new(".")
        } else {
            relative.as_os_str()
        };
        let display_path = self.path.join(relative);
        openat2_file(
            self.file.as_raw_fd(),
            path.as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            &display_path,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NodeIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
}

impl NodeIdentity {
    pub(super) fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileSnapshot {
    pub(super) node: NodeIdentity,
    pub(super) size: u64,
    pub(super) ctime: i64,
    pub(super) ctime_nsec: i64,
    pub(super) mode: u32,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) links: u64,
}

impl FileSnapshot {
    pub(super) fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            node: NodeIdentity::from_metadata(metadata),
            size: metadata.len(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
        }
    }
}
