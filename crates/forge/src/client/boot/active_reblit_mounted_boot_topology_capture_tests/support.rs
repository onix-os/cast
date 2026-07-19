use std::{
    ffi::CString,
    fs::{self, File},
    io,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
    path::PathBuf,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use super::super::{
    PreparedActiveReblitMountedBootTopology,
    capture::{ActiveReblitMountedBootTopologyCaptureError, FixtureMountInfoFeed},
};
use crate::{
    Installation,
    linux_fs::{
        mount_namespace::{FixtureMountNamespaceTree, PreparedMountNamespaceAnchor},
        sysfs_identity::FixtureSysfsTree,
    },
};

pub(super) const PARTUUID: &str = "5e85a94f-b115-41c5-9d72-9d23958b5edc";
pub(super) const CHANGED_PARTUUID: &str = "6e85a94f-b115-41c5-9d72-9d23958b5edc";
pub(super) const MOUNT_POINT: &str = "/firmware";
pub(super) const DISK_SEQUENCE: u64 = 712_345;

const CONTEXT_NAME: &str = "fixture-mount-context";
const SYSFS_NAME: &str = "fixture-sysfs";
const OUTSIDE_BYTES: &[u8] = b"outside mounted-topology fixture remains unchanged\n";

pub(super) fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

pub(in crate::client) struct AliasFixture {
    _temporary: TempDir,
    pub(super) installation: Installation,
    source: PathBuf,
    context_parent: PathBuf,
    context: PathBuf,
    attachment: PathBuf,
    sysfs_tree: FixtureSysfsTree,
    partition_uevent: PathBuf,
    outside: PathBuf,
    feed: FixtureMountInfoFeed,
    destination_device: u64,
    destination_inode: u64,
    device_major: u32,
    device_minor: u32,
}

impl AliasFixture {
    pub(in crate::client) fn stable() -> io::Result<Self> {
        let temporary = tempfile::tempdir()?;
        let outside = temporary.path().join("outside-sentinel");
        fs::write(&outside, OUTSIDE_BYTES)?;

        let installation_root = temporary.path().join("installation");
        fs::create_dir(&installation_root)?;
        fs::set_permissions(&installation_root, fs::Permissions::from_mode(0o755))?;
        let installation = Installation::open(&installation_root, None)
            .map_err(|source| io::Error::other(format!("fixture installation admission failed: {source}")))?;
        let source_directory = installation_root.join("etc/cast");
        fs::create_dir_all(&source_directory)?;
        for directory in [installation_root.join("etc"), source_directory] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o755))?;
        }
        let source = installation_root.join("etc/cast/boot-topology.glu");
        write_source(&source, PARTUUID)?;

        let context_parent = temporary.path().join("context-parent");
        let context = context_parent.join(CONTEXT_NAME);
        fs::create_dir_all(context.join("ns"))?;
        fs::write(context.join("ns/mnt"), b"ordinary fixture namespace marker\n")?;
        let attachment = context.join("root/firmware");
        fs::create_dir_all(&attachment)?;
        let metadata = fs::symlink_metadata(&attachment)?;
        let destination_device = metadata.dev();
        let destination_inode = metadata.ino();
        let raw: nix::libc::dev_t = destination_device;
        let device_major = nix::libc::major(raw);
        let device_minor = nix::libc::minor(raw);
        if nix::libc::makedev(device_major, device_minor) != raw || destination_inode == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture destination lacks canonical nonzero Linux identity",
            ));
        }

        let sysfs_parent = temporary.path().join("sysfs-parent");
        fs::create_dir(&sysfs_parent)?;
        let (sysfs_tree, partition_uevent) = build_sysfs_fixture(&sysfs_parent, device_major, device_minor, PARTUUID)?;
        let mountinfo = mountinfo_record(destination_inode, device_major, device_minor);
        let feed = FixtureMountInfoFeed::new(mountinfo);

        Ok(Self {
            _temporary: temporary,
            installation,
            source,
            context_parent,
            context,
            attachment,
            sysfs_tree,
            partition_uevent,
            outside,
            feed,
            destination_device,
            destination_inode,
            device_major,
            device_minor,
        })
    }

    pub(super) fn prepare(
        &self,
    ) -> Result<PreparedActiveReblitMountedBootTopology, ActiveReblitMountedBootTopologyCaptureError> {
        self.prepare_until(deadline())
    }

    pub(in crate::client) fn prepare_until(
        &self,
        deadline: Instant,
    ) -> Result<PreparedActiveReblitMountedBootTopology, ActiveReblitMountedBootTopologyCaptureError> {
        PreparedActiveReblitMountedBootTopology::prepare_fixture_until(
            &self.installation,
            self.prepared_anchor().expect("fixture anchor preparation must succeed"),
            &self.sysfs_tree,
            self.feed.clone(),
            deadline,
        )
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.installation
    }

    pub(super) fn feed(&self) -> FixtureMountInfoFeed {
        self.feed.clone()
    }

    pub(super) fn destination_identity(&self) -> (u64, u64) {
        (self.destination_device, self.destination_inode)
    }

    pub(super) fn device(&self) -> (u32, u32) {
        (self.device_major, self.device_minor)
    }

    pub(super) fn change_intent_source(&self) -> io::Result<()> {
        write_source(&self.source, CHANGED_PARTUUID)
    }

    pub(super) fn replace_attachment_identity(&self) -> io::Result<()> {
        let displaced = self.context.join("root/displaced-firmware");
        fs::rename(&self.attachment, displaced)?;
        fs::create_dir(&self.attachment)
    }

    pub(super) fn replace_namespace_identity(&self) -> io::Result<()> {
        let marker = self.context.join("ns/mnt");
        fs::rename(&marker, self.context.join("ns/displaced-mnt"))?;
        fs::write(marker, b"replacement ordinary fixture namespace marker\n")
    }

    pub(super) fn change_sysfs_partuuid(&self) -> io::Result<()> {
        fs::write(
            &self.partition_uevent,
            partition_event(self.device_major, self.device_minor, CHANGED_PARTUUID),
        )
    }

    pub(super) fn replace_mountinfo_with_wrong_mount_id(&self) {
        self.feed.replace(mountinfo_record(
            self.destination_inode.saturating_add(1),
            self.device_major,
            self.device_minor,
        ));
    }

    pub(in crate::client) fn assert_outside_unchanged(&self) {
        assert_eq!(fs::read(&self.outside).unwrap(), OUTSIDE_BYTES);
    }

    fn prepared_anchor(&self) -> io::Result<PreparedMountNamespaceAnchor> {
        FixtureMountNamespaceTree::admit(
            File::open(&self.context_parent)?,
            CString::new(CONTEXT_NAME).expect("fixed context name contains no NUL"),
        )?
        .prepare()
    }
}

fn write_source(path: &PathBuf, partuuid: &str) -> io::Result<()> {
    fs::write(
        path,
        format!(
            "let cast = import! cast.boot_topology.v2\ncast.boot_topology.aliases_esp {{ partuuid = \"{partuuid}\", mount_point = \"{MOUNT_POINT}\" }}\n"
        ),
    )?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))
}

fn build_sysfs_fixture(
    parent: &PathBuf,
    partition_major: u32,
    partition_minor: u32,
    partuuid: &str,
) -> io::Result<(FixtureSysfsTree, PathBuf)> {
    let root = parent.join(SYSFS_NAME);
    let disk = root.join("devices/platform/fixture/block/fixture-disk");
    let partition = disk.join("fixture-diskp1");
    fs::create_dir_all(root.join("dev/block"))?;
    fs::create_dir_all(&partition)?;

    let (disk_major, disk_minor) = distinct_parent_device(partition_major, partition_minor);
    fs::write(disk.join("dev"), format!("{disk_major}:{disk_minor}\n"))?;
    fs::write(
        disk.join("uevent"),
        format!(
            "MAJOR={disk_major}\nMINOR={disk_minor}\nDEVNAME=fixture-disk\nDEVTYPE=disk\nDISKSEQ={DISK_SEQUENCE}\n"
        ),
    )?;
    symlink("../../../class/block", disk.join("subsystem"))?;

    fs::write(partition.join("dev"), format!("{partition_major}:{partition_minor}\n"))?;
    fs::write(partition.join("partition"), b"1\n")?;
    let partition_uevent = partition.join("uevent");
    fs::write(
        &partition_uevent,
        partition_event(partition_major, partition_minor, partuuid),
    )?;
    symlink("../../../class/block", partition.join("subsystem"))?;
    symlink(
        "../../devices/platform/fixture/block/fixture-disk/fixture-diskp1",
        root.join("dev/block")
            .join(format!("{partition_major}:{partition_minor}")),
    )?;

    let admitted = FixtureSysfsTree::admit(
        File::open(parent)?,
        CString::new(SYSFS_NAME).expect("fixed sysfs name contains no NUL"),
    )?;
    Ok((admitted, partition_uevent))
}

fn partition_event(major: u32, minor: u32, partuuid: &str) -> Vec<u8> {
    format!(
        "MAJOR={major}\nMINOR={minor}\nDEVNAME=fixture-diskp1\nDEVTYPE=partition\nPARTN=1\nPARTUUID={partuuid}\nDISKSEQ={DISK_SEQUENCE}\n"
    )
    .into_bytes()
}

fn distinct_parent_device(major: u32, minor: u32) -> (u32, u32) {
    if minor < u32::MAX {
        (major, minor + 1)
    } else if major < u32::MAX {
        (major + 1, 0)
    } else {
        (major - 1, 0)
    }
}

fn mountinfo_record(mount_id: u64, major: u32, minor: u32) -> Vec<u8> {
    format!("{mount_id} 1 {major}:{minor} / {MOUNT_POINT} rw,nosuid - vfat ignored rw\n").into_bytes()
}
