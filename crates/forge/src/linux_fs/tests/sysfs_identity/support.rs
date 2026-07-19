use std::{
    ffi::{CString, OsStr},
    fs::{self, File},
    io,
    os::unix::{ffi::OsStrExt as _, fs::symlink},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

pub(super) const PARTITION_MAJOR: u32 = u32::MAX;
pub(super) const PARTITION_MINOR: u32 = u32::MAX - 1;
pub(super) const SIBLING_PARTITION_MINOR: u32 = u32::MAX - 3;
pub(super) const DISK_MAJOR: u32 = u32::MAX;
pub(super) const DISK_MINOR: u32 = u32::MAX - 2;
pub(super) const PARTITION_NUMBER: u32 = 7;
pub(super) const SIBLING_PARTITION_NUMBER: u32 = 8;
pub(super) const DISK_SEQUENCE: u64 = u64::MAX - 7;
pub(super) const PARTITION_UUID: &str = "5e85a94f-b115-41c5-9d72-9d23958b5edc";
pub(super) const SIBLING_PARTITION_UUID: &str = "6e85a94f-b115-41c5-9d72-9d23958b5edc";

const ROOT_NAME: &str = "synthetic-sysfs";
const DISK_NAME: &str = "fixture-disk";
const PARTITION_NAME: &str = "fixture-diskp7";
const SIBLING_PARTITION_NAME: &str = "fixture-diskp8";
const OUTSIDE_CONTENTS: &[u8] = b"outside fixture remains unchanged\n";

static DISPLACEMENT: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FixtureEntry {
    Lookup,
    PartitionDirectory,
    PartitionDevice,
    PartitionNumber,
    PartitionEvent,
    PartitionSubsystem,
    IntermediateSubsystem,
    DiskDirectory,
    DiskDevice,
    DiskEvent,
    DiskSubsystem,
}

pub(super) struct SyntheticSysfs {
    temporary: tempfile::TempDir,
    root: PathBuf,
    disk: PathBuf,
    intermediate: PathBuf,
    partition: PathBuf,
    outside: PathBuf,
    disk_sequence: Option<u64>,
}

impl SyntheticSysfs {
    pub(super) fn stable() -> io::Result<Self> {
        Self::with_disk_sequence(Some(DISK_SEQUENCE))
    }

    pub(super) fn without_disk_sequence() -> io::Result<Self> {
        Self::with_disk_sequence(None)
    }

    fn with_disk_sequence(disk_sequence: Option<u64>) -> io::Result<Self> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join(ROOT_NAME);
        let disk = root
            .join("devices")
            .join("platform")
            .join("fixture-fabric")
            .join("block")
            .join(DISK_NAME);
        let intermediate = disk.join("fixture-bridge");
        let partition = intermediate.join(PARTITION_NAME);
        let outside = temporary.path().join("outside-sentinel");
        fs::write(&outside, OUTSIDE_CONTENTS)?;

        let fixture = Self {
            temporary,
            root,
            disk,
            intermediate,
            partition,
            outside,
            disk_sequence,
        };
        fixture.populate()?;
        Ok(fixture)
    }

    pub(super) fn admission(&self) -> io::Result<(File, CString)> {
        Ok((
            File::open(self.temporary.path())?,
            CString::new(ROOT_NAME).expect("fixed fixture root contains no NUL"),
        ))
    }

    pub(super) fn normalized_device_path(&self) -> &[u8] {
        b"devices/platform/fixture-fabric/block/fixture-disk/fixture-bridge/fixture-diskp7"
    }

    pub(super) fn logical_device_path(&self) -> &[u8] {
        b"/sys/devices/platform/fixture-fabric/block/fixture-disk/fixture-bridge/fixture-diskp7"
    }

    pub(super) fn entry(&self, entry: FixtureEntry) -> PathBuf {
        match entry {
            FixtureEntry::Lookup => self.root.join("dev").join("block").join(self.device_component()),
            FixtureEntry::PartitionDirectory => self.partition.clone(),
            FixtureEntry::PartitionDevice => self.partition.join("dev"),
            FixtureEntry::PartitionNumber => self.partition.join("partition"),
            FixtureEntry::PartitionEvent => self.partition.join("uevent"),
            FixtureEntry::PartitionSubsystem => self.partition.join("subsystem"),
            FixtureEntry::IntermediateSubsystem => self.intermediate.join("subsystem"),
            FixtureEntry::DiskDirectory => self.disk.clone(),
            FixtureEntry::DiskDevice => self.disk.join("dev"),
            FixtureEntry::DiskEvent => self.disk.join("uevent"),
            FixtureEntry::DiskSubsystem => self.disk.join("subsystem"),
        }
    }

    pub(super) fn remove(&self, entry: FixtureEntry) -> io::Result<()> {
        let path = self.entry(entry);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    }

    pub(super) fn replace_regular(&self, entry: FixtureEntry, contents: &[u8]) -> io::Result<()> {
        let path = self.entry(entry);
        self.displace(&path)?;
        fs::write(path, contents)
    }

    pub(super) fn overwrite_regular(&self, entry: FixtureEntry, contents: &[u8]) -> io::Result<()> {
        fs::write(self.entry(entry), contents)
    }

    pub(super) fn replace_symlink(&self, entry: FixtureEntry, target: &[u8]) -> io::Result<()> {
        let path = self.entry(entry);
        self.displace(&path)?;
        symlink(OsStr::from_bytes(target), path)
    }

    pub(super) fn replace_fifo(&self, entry: FixtureEntry) -> io::Result<()> {
        let path = self.entry(entry);
        self.displace(&path)?;
        let encoded = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "fixture FIFO path contains NUL"))?;
        // SAFETY: `encoded` is a live NUL-terminated path below the fixture's
        // temporary root. The mode describes a user-owned FIFO only.
        if unsafe { nix::libc::mkfifo(encoded.as_ptr(), 0o600) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn replace_partition_directory(&self) -> io::Result<()> {
        self.displace(&self.partition)?;
        self.populate_partition()
    }

    pub(super) fn replace_intermediate_directory(&self) -> io::Result<()> {
        self.displace(&self.intermediate)?;
        fs::create_dir(&self.intermediate)?;
        self.populate_intermediate()?;
        self.populate_partition()
    }

    pub(super) fn replace_disk_directory(&self) -> io::Result<()> {
        self.displace(&self.disk)?;
        fs::create_dir(&self.disk)?;
        self.populate_disk()?;
        fs::create_dir(&self.intermediate)?;
        self.populate_intermediate()?;
        self.populate_partition()
    }

    pub(super) fn replace_root_directory(&self) -> io::Result<()> {
        self.displace(&self.root)?;
        self.populate()
    }

    pub(super) fn replace_root_with_symlink(&self) -> io::Result<()> {
        let displaced = self.displace(&self.root)?;
        let target = displaced
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "displaced fixture root has no basename"))?;
        symlink(target, &self.root)
    }

    pub(super) fn assert_outside_unchanged(&self) {
        assert_eq!(fs::read(&self.outside).unwrap(), OUTSIDE_CONTENTS);
        assert!(fs::symlink_metadata(&self.outside).unwrap().file_type().is_file());
    }

    pub(super) fn add_nearer_block_ancestor(&self, event: &[u8]) -> io::Result<()> {
        self.replace_symlink(FixtureEntry::IntermediateSubsystem, b"../../../class/block")?;
        fs::write(self.intermediate.join("dev"), self.disk_device_contents())?;
        fs::write(self.intermediate.join("uevent"), event)
    }

    pub(super) fn add_sibling_partition(&self) -> io::Result<()> {
        let sibling = self.intermediate.join(SIBLING_PARTITION_NAME);
        fs::create_dir(&sibling)?;
        fs::write(
            sibling.join("dev"),
            format!("{PARTITION_MAJOR}:{SIBLING_PARTITION_MINOR}\n"),
        )?;
        fs::write(sibling.join("partition"), format!("{SIBLING_PARTITION_NUMBER}\n"))?;
        let mut event = format!(
            "MAJOR={PARTITION_MAJOR}\nMINOR={SIBLING_PARTITION_MINOR}\nDEVNAME={SIBLING_PARTITION_NAME}\nDEVTYPE=partition\nPARTN={SIBLING_PARTITION_NUMBER}\nPARTUUID={SIBLING_PARTITION_UUID}\n"
        )
        .into_bytes();
        if let Some(sequence) = self.disk_sequence {
            event.extend_from_slice(format!("DISKSEQ={sequence}\n").as_bytes());
        }
        fs::write(sibling.join("uevent"), event)?;
        symlink("../../../class/block", sibling.join("subsystem"))?;

        let target =
            format!("../../devices/platform/fixture-fabric/block/{DISK_NAME}/fixture-bridge/{SIBLING_PARTITION_NAME}");
        symlink(
            target,
            self.root
                .join("dev")
                .join("block")
                .join(format!("{PARTITION_MAJOR}:{SIBLING_PARTITION_MINOR}")),
        )
    }

    pub(super) fn lookup_target(&self) -> Vec<u8> {
        format!("../../{}", String::from_utf8_lossy(self.normalized_device_path())).into_bytes()
    }

    fn populate(&self) -> io::Result<()> {
        fs::create_dir_all(self.root.join("dev").join("block"))?;
        fs::create_dir_all(&self.disk)?;
        self.populate_disk()?;
        fs::create_dir(&self.intermediate)?;
        self.populate_intermediate()?;
        self.populate_partition()?;
        symlink(
            OsStr::from_bytes(&self.lookup_target()),
            self.root.join("dev").join("block").join(self.device_component()),
        )
    }

    fn populate_disk(&self) -> io::Result<()> {
        fs::write(self.disk.join("dev"), self.disk_device_contents())?;
        fs::write(self.disk.join("uevent"), self.disk_event_contents())?;
        symlink("../../../class/block", self.disk.join("subsystem"))
    }

    fn populate_intermediate(&self) -> io::Result<()> {
        symlink("../../../bus/platform", self.intermediate.join("subsystem"))
    }

    fn populate_partition(&self) -> io::Result<()> {
        fs::create_dir(&self.partition)?;
        fs::write(self.partition.join("dev"), self.partition_device_contents())?;
        fs::write(self.partition.join("partition"), format!("{PARTITION_NUMBER}\n"))?;
        fs::write(self.partition.join("uevent"), self.partition_event_contents())?;
        symlink("../../../class/block", self.partition.join("subsystem"))
    }

    fn partition_device_contents(&self) -> Vec<u8> {
        format!("{PARTITION_MAJOR}:{PARTITION_MINOR}\n").into_bytes()
    }

    fn disk_device_contents(&self) -> Vec<u8> {
        format!("{DISK_MAJOR}:{DISK_MINOR}\n").into_bytes()
    }

    fn partition_event_contents(&self) -> Vec<u8> {
        let mut bytes = format!(
            "MAJOR={PARTITION_MAJOR}\nMINOR={PARTITION_MINOR}\nDEVNAME={PARTITION_NAME}\nDEVTYPE=partition\nPARTN={PARTITION_NUMBER}\nPARTUUID={PARTITION_UUID}\n"
        )
        .into_bytes();
        if let Some(sequence) = self.disk_sequence {
            bytes.extend_from_slice(format!("DISKSEQ={sequence}\n").as_bytes());
        }
        bytes
    }

    fn disk_event_contents(&self) -> Vec<u8> {
        let mut bytes =
            format!("MAJOR={DISK_MAJOR}\nMINOR={DISK_MINOR}\nDEVNAME={DISK_NAME}\nDEVTYPE=disk\n").into_bytes();
        if let Some(sequence) = self.disk_sequence {
            bytes.extend_from_slice(format!("DISKSEQ={sequence}\n").as_bytes());
        }
        bytes
    }

    fn device_component(&self) -> String {
        format!("{PARTITION_MAJOR}:{PARTITION_MINOR}")
    }

    fn displace(&self, path: &Path) -> io::Result<PathBuf> {
        let serial = DISPLACEMENT.fetch_add(1, Ordering::Relaxed);
        let file_name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "fixture entry has no basename"))?;
        let mut displaced_name = file_name.to_os_string();
        displaced_name.push(format!(".displaced-{serial}"));
        let displaced = path.with_file_name(displaced_name);
        fs::rename(path, &displaced)?;
        Ok(displaced)
    }
}
