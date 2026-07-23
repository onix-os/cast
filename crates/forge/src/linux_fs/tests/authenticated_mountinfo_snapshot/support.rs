use std::{
    ffi::CString,
    fs, io,
    path::PathBuf,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use super::super::super::mount_namespace::{FixtureMountNamespaceTree, PreparedMountNamespaceAnchor};

pub(super) const RECORD: &[u8] = b"41 1 259:7 / /synthetic/firmware-attachment rw,nosuid - vfat ignored rw\n";

const TREE_NAME: &str = "synthetic-mountinfo-context";
const MARKER: &[u8] = b"synthetic mount namespace marker\n";
const OUTSIDE: &[u8] = b"outside mountinfo fixture remains unchanged\n";

pub(super) fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

pub(super) struct SyntheticMountInfoContext {
    temporary: TempDir,
    parent: PathBuf,
    tree: PathBuf,
    outside: PathBuf,
}

impl SyntheticMountInfoContext {
    pub(super) fn stable() -> io::Result<Self> {
        let temporary = tempfile::tempdir()?;
        let parent = temporary.path().join("admission-parent");
        let tree = parent.join(TREE_NAME);
        let outside = temporary.path().join("outside-sentinel");
        fs::create_dir(&parent)?;
        fs::write(&outside, OUTSIDE)?;
        let fixture = Self {
            temporary,
            parent,
            tree,
            outside,
        };
        fixture.populate()?;
        Ok(fixture)
    }

    pub(super) fn prepared(&self) -> io::Result<PreparedMountNamespaceAnchor> {
        let parent = fs::File::open(&self.parent)?;
        let name = CString::new(TREE_NAME).expect("fixed synthetic tree name contains no NUL");
        FixtureMountNamespaceTree::admit(parent, name)?.prepare()
    }

    pub(super) fn replace_tree_identity(&self) -> io::Result<()> {
        fs::rename(&self.tree, self.parent.join("displaced-tree"))?;
        self.populate()
    }

    pub(super) fn replace_namespace_identity(&self) -> io::Result<()> {
        let path = self.tree.join("ns").join("mnt");
        fs::rename(&path, self.tree.join("ns").join("displaced-mnt"))?;
        fs::write(path, MARKER)
    }

    pub(super) fn replace_root_identity(&self) -> io::Result<()> {
        let path = self.tree.join("root");
        fs::rename(&path, self.tree.join("displaced-root"))?;
        fs::create_dir(path)
    }

    pub(super) fn assert_outside_unchanged(&self) {
        assert_eq!(fs::read(&self.outside).unwrap(), OUTSIDE);
        assert!(self.temporary.path().is_dir());
    }

    fn populate(&self) -> io::Result<()> {
        fs::create_dir(&self.tree)?;
        fs::create_dir(self.tree.join("ns"))?;
        fs::write(self.tree.join("ns").join("mnt"), MARKER)?;
        fs::create_dir(self.tree.join("root"))
    }
}

pub(super) fn assert_error_kind(result: io::Result<impl Sized>, kind: io::ErrorKind) {
    let error = match result {
        Ok(_) => panic!("mountinfo snapshot operation unexpectedly succeeded"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), kind, "unexpected error: {error}");
}
