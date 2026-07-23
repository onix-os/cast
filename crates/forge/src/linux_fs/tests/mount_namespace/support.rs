use std::{
    ffi::CString,
    fs, io,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
};

use tempfile::TempDir;

const TREE_NAME: &str = "fixture-mount-namespace";
const NAMESPACE_DIRECTORY_NAME: &str = "ns";
const NAMESPACE_MARKER_NAME: &str = "mnt";
const TASK_ROOT_NAME: &str = "root";
const MARKER_CONTENTS: &[u8] = b"synthetic namespace marker contents are not authority\n";
const OUTSIDE_CONTENTS: &[u8] = b"outside mount-namespace fixture remains unchanged\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FixtureEntry {
    Tree,
    NamespaceDirectory,
    NamespaceMarker,
    TaskRoot,
}

pub(super) struct SyntheticMountNamespace {
    temporary: TempDir,
    parent: PathBuf,
    tree: PathBuf,
    outside: PathBuf,
}

impl SyntheticMountNamespace {
    pub(super) fn stable() -> io::Result<Self> {
        let temporary = tempfile::tempdir()?;
        let parent = temporary.path().join("admission-parent");
        let tree = parent.join(TREE_NAME);
        let outside = temporary.path().join("outside-sentinel");
        fs::create_dir(&parent)?;
        fs::write(&outside, OUTSIDE_CONTENTS)?;

        let fixture = Self {
            temporary,
            parent,
            tree,
            outside,
        };
        fixture.populate_tree()?;
        Ok(fixture)
    }

    pub(super) fn with_attachment(components: &[&str]) -> io::Result<Self> {
        if components.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "synthetic attachment needs at least one component",
            ));
        }
        let fixture = Self::stable()?;
        let mut parent = fixture.entry(FixtureEntry::TaskRoot);
        for component in components {
            parent.push(component);
            fs::create_dir(&parent)?;
        }
        Ok(fixture)
    }

    pub(super) fn admission(&self) -> io::Result<(fs::File, CString)> {
        Ok((
            fs::File::open(&self.parent)?,
            CString::new(TREE_NAME).expect("fixed fixture tree name contains no NUL"),
        ))
    }

    pub(super) fn identity(&self, entry: FixtureEntry) -> io::Result<(u64, u64)> {
        let metadata = fs::symlink_metadata(self.entry(entry))?;
        Ok((metadata.dev(), metadata.ino()))
    }

    pub(super) fn overwrite_marker_contents(&self, contents: &[u8]) -> io::Result<()> {
        fs::write(self.entry(FixtureEntry::NamespaceMarker), contents)
    }

    pub(super) fn attachment_identity(&self, components: &[&str], index: usize) -> io::Result<(u64, u64)> {
        let metadata = fs::symlink_metadata(self.attachment_entry(components, index)?)?;
        Ok((metadata.dev(), metadata.ino()))
    }

    pub(super) fn remove_attachment(&self, components: &[&str], index: usize) -> io::Result<()> {
        remove_entry(&self.attachment_entry(components, index)?)
    }

    pub(super) fn replace_attachment_identity(&self, components: &[&str], index: usize) -> io::Result<()> {
        let path = self.attachment_entry(components, index)?;
        let displaced = self
            .entry(FixtureEntry::TaskRoot)
            .join(format!("displaced-attachment-{index}"));
        fs::rename(&path, displaced)?;
        let mut parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "synthetic attachment has no parent"))?
            .to_path_buf();
        for component in &components[index..] {
            parent.push(component);
            fs::create_dir(&parent)?;
        }
        Ok(())
    }

    pub(super) fn replace_attachment_regular(&self, components: &[&str], index: usize) -> io::Result<()> {
        let path = self.attachment_entry(components, index)?;
        remove_entry(&path)?;
        fs::write(path, b"synthetic wrong-kind attachment\n")
    }

    pub(super) fn replace_attachment_symlink(&self, components: &[&str], index: usize) -> io::Result<()> {
        let path = self.attachment_entry(components, index)?;
        remove_entry(&path)?;
        std::os::unix::fs::symlink("missing-synthetic-attachment", path)
    }

    pub(super) fn replace_attachment_fifo(&self, components: &[&str], index: usize) -> io::Result<()> {
        let path = self.attachment_entry(components, index)?;
        remove_entry(&path)?;
        let encoded = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "fixture FIFO path contains NUL"))?;
        // SAFETY: this creates a FIFO only inside the fixture's private
        // temporary directory; no descriptor for the FIFO is opened.
        let status = unsafe { nix::libc::mkfifo(encoded.as_ptr(), 0o600) };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn remove(&self, entry: FixtureEntry) -> io::Result<()> {
        remove_entry(&self.entry(entry))
    }

    pub(super) fn replace_regular(&self, entry: FixtureEntry, contents: &[u8]) -> io::Result<()> {
        let path = self.entry(entry);
        remove_entry(&path)?;
        fs::write(path, contents)
    }

    pub(super) fn replace_directory(&self, entry: FixtureEntry) -> io::Result<()> {
        let path = self.entry(entry);
        remove_entry(&path)?;
        fs::create_dir(path)
    }

    pub(super) fn replace_symlink(&self, entry: FixtureEntry) -> io::Result<()> {
        let path = self.entry(entry);
        remove_entry(&path)?;
        std::os::unix::fs::symlink("missing-synthetic-target", path)
    }

    pub(super) fn replace_fifo(&self, entry: FixtureEntry) -> io::Result<()> {
        let path = self.entry(entry);
        remove_entry(&path)?;
        let encoded = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "fixture FIFO path contains NUL"))?;
        // SAFETY: `encoded` is a live NUL-terminated path below the fixture's
        // private temporary directory. No descriptor for the FIFO is opened.
        let status = unsafe { nix::libc::mkfifo(encoded.as_ptr(), 0o600) };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn replace_tree_identity(&self) -> io::Result<()> {
        let displaced = self.parent.join("displaced-tree");
        fs::rename(&self.tree, displaced)?;
        self.populate_tree()
    }

    pub(super) fn replace_namespace_directory_identity(&self) -> io::Result<()> {
        let path = self.entry(FixtureEntry::NamespaceDirectory);
        fs::rename(&path, self.tree.join("displaced-ns"))?;
        fs::create_dir(&path)?;
        fs::write(path.join(NAMESPACE_MARKER_NAME), MARKER_CONTENTS)
    }

    pub(super) fn replace_namespace_marker_identity(&self) -> io::Result<()> {
        let path = self.entry(FixtureEntry::NamespaceMarker);
        fs::rename(&path, self.tree.join(NAMESPACE_DIRECTORY_NAME).join("displaced-mnt"))?;
        fs::write(path, MARKER_CONTENTS)
    }

    pub(super) fn replace_task_root_identity(&self) -> io::Result<()> {
        let path = self.entry(FixtureEntry::TaskRoot);
        fs::rename(&path, self.tree.join("displaced-root"))?;
        fs::create_dir(path)
    }

    pub(super) fn assert_outside_unchanged(&self) {
        assert_eq!(fs::read(&self.outside).unwrap(), OUTSIDE_CONTENTS);
        assert!(self.temporary.path().is_dir());
    }

    pub(super) fn entry(&self, entry: FixtureEntry) -> PathBuf {
        match entry {
            FixtureEntry::Tree => self.tree.clone(),
            FixtureEntry::NamespaceDirectory => self.tree.join(NAMESPACE_DIRECTORY_NAME),
            FixtureEntry::NamespaceMarker => self.tree.join(NAMESPACE_DIRECTORY_NAME).join(NAMESPACE_MARKER_NAME),
            FixtureEntry::TaskRoot => self.tree.join(TASK_ROOT_NAME),
        }
    }

    fn attachment_entry(&self, components: &[&str], index: usize) -> io::Result<PathBuf> {
        if components.is_empty() || index >= components.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "synthetic attachment index is outside its component chain",
            ));
        }
        let mut path = self.entry(FixtureEntry::TaskRoot);
        for component in &components[..=index] {
            path.push(component);
        }
        Ok(path)
    }

    fn populate_tree(&self) -> io::Result<()> {
        fs::create_dir(&self.tree)?;
        fs::create_dir(self.tree.join(NAMESPACE_DIRECTORY_NAME))?;
        fs::write(
            self.tree.join(NAMESPACE_DIRECTORY_NAME).join(NAMESPACE_MARKER_NAME),
            MARKER_CONTENTS,
        )?;
        fs::create_dir(self.tree.join(TASK_ROOT_NAME))
    }
}

fn remove_entry(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}
