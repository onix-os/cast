// This test deliberately exercises raw descriptor boundaries. `std::fs::File`
// keeps those operations explicit instead of attaching path context after the
// descriptor has already been authenticated.
#![allow(clippy::disallowed_types)]

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString},
    fs::{File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Component, Path, PathBuf},
    ptr::NonNull,
};

use elf::{
    ElfBytes,
    abi::{
        DT_NEEDED, DT_SONAME, ELFCOMPRESS_ZLIB, ELFCOMPRESS_ZSTD, EM_X86_64, ET_DYN, ET_EXEC, EV_CURRENT, PT_INTERP,
        PT_LOAD, SHT_PROGBITS,
    },
    endian::{AnyEndian, EndianParse},
    file::Class,
    note::Note,
};
use forge::package::Meta;
use stone::{
    StoneDecodeLimits, StoneDecodedPayload, StoneDigestWriterHasher, StoneHeader, StoneHeaderV1FileType,
    StonePayloadCompression, StonePayloadKind, StonePayloadLayoutFile, StonePayloadLayoutRecord,
    StonePayloadMetaPrimitive, StonePayloadMetaRecord, StonePayloadMetaTag,
    relation::{Dependency, Kind, Provider},
};
use stone_recipe::derivation::{OutputPlan, OutputRelation};

const MAX_FIXTURE_ARTEFACT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FIXTURE_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FIXTURE_CONTENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TRACKED_FIXTURE_BYTES: u64 = 64 * 1024;
const PUBLISHED_ARTEFACT_MODE: u32 = 0o444;
const PUBLISHED_BUNDLE_MODE: u32 = 0o555;
const STAGED_BUNDLE_MODE: u32 = 0o700;

#[derive(Debug, Clone, Copy)]
pub(super) enum BundleRootRole {
    Published,
    Staged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileStamp {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

struct DirectoryStream(NonNull<nix::libc::DIR>);

impl DirectoryStream {
    fn from_file(file: File, fixture: &str, path: &Path) -> Self {
        let descriptor = file.into_raw_fd();
        // SAFETY: `descriptor` is fresh and fdopendir consumes it on success.
        let stream = unsafe { nix::libc::fdopendir(descriptor) };
        match NonNull::new(stream) {
            Some(stream) => Self(stream),
            None => {
                let error = std::io::Error::last_os_error();
                // SAFETY: fdopendir failed, so it did not consume the descriptor.
                unsafe { nix::libc::close(descriptor) };
                panic!("{fixture}: open descriptor-rooted directory stream {path:?}: {error}")
            }
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the live DIR pointer.
        unsafe { nix::libc::closedir(self.0.as_ptr()) };
    }
}

struct BundleDirectory {
    path: PathBuf,
    file: File,
}

impl BundleDirectory {
    fn open(fixture: &str, path: &Path, role: BundleRootRole) -> Self {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(path)
            .unwrap_or_else(|error| {
                panic!("{fixture}: open emitted bundle root {path:?} without following links: {error}")
            });
        let metadata = file
            .metadata()
            .unwrap_or_else(|error| panic!("{fixture}: inspect opened bundle root {path:?}: {error}"));
        assert!(
            metadata.file_type().is_dir(),
            "{fixture}: opened bundle root is not a directory"
        );
        let expected_mode = match role {
            BundleRootRole::Published => PUBLISHED_BUNDLE_MODE,
            BundleRootRole::Staged => STAGED_BUNDLE_MODE,
        };
        assert_eq!(
            metadata.mode() & (nix::libc::S_IFMT | 0o7777),
            nix::libc::S_IFDIR | expected_mode,
            "{fixture}: {role:?} bundle root must have exact mode {expected_mode:o}"
        );
        // SAFETY: geteuid has no preconditions and does not mutate process state.
        assert_eq!(
            metadata.uid(),
            unsafe { nix::libc::geteuid() },
            "{fixture}: bundle root is not owned by the executor"
        );
        assert!(
            metadata.nlink() >= 2,
            "{fixture}: published bundle root has an invalid link count"
        );
        Self {
            path: path.to_owned(),
            file,
        }
    }

    fn stamp(&self, fixture: &str) -> FileStamp {
        FileStamp::from_metadata(
            &self
                .file
                .metadata()
                .unwrap_or_else(|error| panic!("{fixture}: inspect emitted bundle descriptor: {error}")),
        )
    }

    fn names(&self, fixture: &str, maximum: usize) -> Vec<String> {
        let dot = c".";
        // SAFETY: `self.file` is a live directory descriptor and `dot` is a
        // static NUL-terminated component. The returned descriptor is owned.
        let descriptor = unsafe {
            nix::libc::openat(
                self.file.as_raw_fd(),
                dot.as_ptr(),
                nix::libc::O_RDONLY
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK,
            )
        };
        assert!(
            descriptor >= 0,
            "{fixture}: open descriptor-rooted cursor for {:?}: {}",
            self.path,
            std::io::Error::last_os_error()
        );
        // SAFETY: openat returned a fresh owned descriptor.
        let cursor = unsafe { File::from_raw_fd(descriptor) };
        let stream = DirectoryStream::from_file(cursor, fixture, &self.path);
        let mut names = Vec::new();
        names
            .try_reserve(maximum)
            .unwrap_or_else(|error| panic!("{fixture}: reserve bounded bundle inventory: {error}"));
        loop {
            nix::errno::Errno::clear();
            // SAFETY: the live directory stream is exclusively used here.
            let entry = unsafe { nix::libc::readdir(stream.0.as_ptr()) };
            if entry.is_null() {
                let error = nix::errno::Errno::last();
                assert_eq!(
                    error,
                    nix::errno::Errno::UnknownErrno,
                    "{fixture}: enumerate emitted bundle {:?}: {error}",
                    self.path
                );
                break;
            }
            // SAFETY: d_name is NUL-terminated and valid until the next call.
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            assert!(
                names.len() < maximum,
                "{fixture}: bundle enumeration exceeded its expected-plus-one cap of {maximum} entries"
            );
            let name = std::str::from_utf8(bytes)
                .unwrap_or_else(|_| panic!("{fixture}: bundle contains a non-UTF-8 artefact name"));
            assert!(!name.contains('/'), "{fixture}: bundle entry is not a single component");
            names.push(name.to_owned());
        }
        names.sort_unstable();
        names
    }

    fn read_artefact(&self, fixture: &str, name: &str, aggregate_remaining: u64) -> Vec<u8> {
        self.read_artefact_after_authentication(fixture, name, aggregate_remaining, || {})
    }

    fn read_artefact_after_authentication(
        &self,
        fixture: &str,
        name: &str,
        aggregate_remaining: u64,
        checkpoint: impl FnOnce(),
    ) -> Vec<u8> {
        let mut components = Path::new(name).components();
        assert!(
            matches!(components.next(), Some(Component::Normal(component)) if component == std::ffi::OsStr::new(name))
                && components.next().is_none(),
            "{fixture}: expected artefact name is not one normalized component"
        );
        let component = CString::new(name).unwrap_or_else(|_| panic!("{fixture}: artefact name contains NUL"));
        // SAFETY: the parent descriptor is live and `component` is a validated
        // single component. O_NOFOLLOW rejects a substituted final symlink.
        let descriptor = unsafe {
            nix::libc::openat(
                self.file.as_raw_fd(),
                component.as_ptr(),
                nix::libc::O_RDONLY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK
                    | nix::libc::O_NOCTTY,
            )
        };
        assert!(
            descriptor >= 0,
            "{fixture}: open emitted artefact {name:?} without following links: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: openat returned a fresh owned descriptor.
        let mut file = unsafe { File::from_raw_fd(descriptor) };
        let before = file
            .metadata()
            .unwrap_or_else(|error| panic!("{fixture}: inspect opened artefact {name:?}: {error}"));
        assert!(
            before.file_type().is_file(),
            "{fixture}: bundle entry {name:?} is not a regular file"
        );
        assert_eq!(
            before.nlink(),
            1,
            "{fixture}: bundle entry {name:?} must have exactly one hard link"
        );
        // SAFETY: geteuid has no preconditions and does not mutate process state.
        assert_eq!(
            before.uid(),
            unsafe { nix::libc::geteuid() },
            "{fixture}: bundle entry {name:?} is not owned by the executor"
        );
        assert_eq!(
            before.mode() & (nix::libc::S_IFMT | 0o7777),
            nix::libc::S_IFREG | PUBLISHED_ARTEFACT_MODE,
            "{fixture}: published artefact {name:?} must be sealed to mode {PUBLISHED_ARTEFACT_MODE:o}"
        );
        assert!(before.size() > 0, "{fixture}: bundle entry {name:?} is empty");
        assert!(
            before.size() <= MAX_FIXTURE_ARTEFACT_BYTES,
            "{fixture}: bundle entry {name:?} exceeds the fixture boundary"
        );
        assert!(
            before.size() <= aggregate_remaining,
            "{fixture}: authenticated bundle sizes exceed the {MAX_FIXTURE_BUNDLE_BYTES}-byte aggregate boundary"
        );
        let stamp = FileStamp::from_metadata(&before);
        checkpoint();
        let bytes = read_bounded(fixture, &format!("emitted artefact {name:?}"), &mut file, before.size());
        assert_eq!(
            u64::try_from(bytes.len()).unwrap(),
            before.size(),
            "{fixture}: artefact {name:?} length changed while reading"
        );
        let after = file
            .metadata()
            .unwrap_or_else(|error| panic!("{fixture}: reinspect opened artefact {name:?}: {error}"));
        assert_eq!(
            FileStamp::from_metadata(&after),
            stamp,
            "{fixture}: artefact {name:?} changed while reading"
        );
        bytes
    }

    fn snapshot(self, fixture: &str, expected: &BTreeSet<String>) -> BTreeMap<String, Vec<u8>> {
        let maximum = expected
            .len()
            .checked_add(1)
            .expect("fixture bundle entry cap overflow");
        let before = self.stamp(fixture);
        let first = self.names(fixture, maximum);
        let between = self.stamp(fixture);
        assert_eq!(before, between, "{fixture}: bundle directory changed during inventory");
        assert_eq!(
            first.iter().map(String::as_str).collect::<BTreeSet<_>>(),
            expected.iter().map(String::as_str).collect::<BTreeSet<_>>(),
            "{fixture}: incomplete or surplus emitted bundle"
        );

        let mut aggregate = 0u64;
        let mut artefacts = BTreeMap::new();
        for name in expected {
            let remaining = MAX_FIXTURE_BUNDLE_BYTES
                .checked_sub(aggregate)
                .expect("fixture aggregate was checked after every artefact");
            let bytes = self.read_artefact(fixture, name, remaining);
            aggregate = aggregate
                .checked_add(u64::try_from(bytes.len()).unwrap())
                .expect("fixture bundle byte sum overflow");
            assert!(
                aggregate <= MAX_FIXTURE_BUNDLE_BYTES,
                "{fixture}: bundle exceeds its {MAX_FIXTURE_BUNDLE_BYTES}-byte aggregate boundary"
            );
            assert!(artefacts.insert(name.clone(), bytes).is_none());
        }

        let second = self.names(fixture, maximum);
        let after = self.stamp(fixture);
        assert_eq!(
            between, after,
            "{fixture}: bundle directory changed while reading artefacts"
        );
        assert_eq!(
            first, second,
            "{fixture}: bundle inventory changed while reading artefacts"
        );
        artefacts
    }
}

#[test]
fn authenticated_artefact_size_is_the_authoritative_read_boundary() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(STAGED_BUNDLE_MODE)).unwrap();
    let artefact_path = root.path().join("fixture.stone");
    let mut writer = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&artefact_path)
        .unwrap();
    writer.write_all(b"a").unwrap();
    writer.sync_all().unwrap();
    std::fs::set_permissions(&artefact_path, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();

    let directory = BundleDirectory::open("growth", root.path(), BundleRootRole::Staged);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        directory.read_artefact_after_authentication("growth", "fixture.stone", 1, || {
            writer.seek(SeekFrom::End(0)).unwrap();
            writer.write_all(b"b").unwrap();
            writer.sync_all().unwrap();
        });
    }))
    .expect_err("growing an artefact after authentication must fail");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic");
    assert!(
        message.contains("exceeds its 1-byte boundary"),
        "growth must be rejected by the authenticated-size read cap, not only by a later stamp check: {message}"
    );
}

fn read_bounded(fixture: &str, role: &str, file: &mut File, maximum: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    let reserve = usize::try_from(maximum.min(64 * 1024)).unwrap();
    bytes
        .try_reserve(reserve)
        .unwrap_or_else(|error| panic!("{fixture}: reserve bounded {role} buffer: {error}"));
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .unwrap_or_else(|error| panic!("{fixture}: read bounded {role}: {error}"));
    assert!(
        u64::try_from(bytes.len()).unwrap() <= maximum,
        "{fixture}: {role} exceeds its {maximum}-byte boundary"
    );
    bytes
}

include!("bundle/elf.rs");
include!("bundle/fixture_expectations.rs");
include!("bundle/manifest.rs");
include!("bundle/package_decode.rs");
include!("bundle/package_metadata.rs");
include!("bundle/tracked_sources.rs");
include!("bundle/desktop_integration.rs");
include!("bundle/font_family.rs");
include!("bundle/gettext_localization.rs");
include!("bundle/go_module.rs");
include!("bundle/system_integration_assets.rs");
