// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

// This test deliberately exercises raw descriptor boundaries. `std::fs::File`
// keeps those operations explicit instead of attaching path context after the
// descriptor has already been authenticated.
#![allow(clippy::disallowed_types)]

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString},
    fs::{File, Metadata, OpenOptions},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Component, Path, PathBuf},
    ptr::NonNull,
};

use elf::{
    ElfBytes,
    abi::{DT_NEEDED, DT_SONAME, EM_X86_64, ET_DYN, ET_EXEC, EV_CURRENT, PT_INTERP, PT_LOAD},
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
        let bytes = read_bounded(
            fixture,
            &format!("emitted artefact {name:?}"),
            &mut file,
            MAX_FIXTURE_ARTEFACT_BYTES,
        );
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

#[derive(Debug)]
struct PackageImage {
    records: Vec<StonePayloadMetaRecord>,
    meta: Meta,
    layouts: BTreeMap<String, StonePayloadLayoutRecord>,
    content: BTreeMap<u128, Vec<u8>>,
}

/// Decode and verify the complete emitted bundle for one contentful execution
/// fixture. This deliberately does more than prove that Stone can parse its
/// own output: it ties metadata, layouts, indices, content, manifests, the
/// frozen plan, and the checked-in source fixture together.
pub(super) fn assert_fixture_bundle(name: &str, planned: &super::super::Planned, root: &Path, role: BundleRootRole) {
    assert!(
        matches!(name, "autotools" | "cargo" | "cmake" | "custom" | "meson" | "split"),
        "unknown contentful execution fixture {name:?}"
    );
    planned
        .plan
        .validate()
        .unwrap_or_else(|error| panic!("{name}: validate the frozen plan before inspecting its bundle: {error}"));

    let package_name = format!("cast-{name}-fixture");
    assert_eq!(planned.plan.package.name, package_name);
    assert_eq!(planned.plan.package.version, "1.0.0");
    assert_eq!(planned.plan.package.source_release, 1);
    assert_eq!(planned.plan.package.build_release, 1);
    assert_eq!(planned.plan.package.architecture, "x86_64");
    assert_eq!(
        planned.plan.package.homepage,
        format!("https://fixtures.invalid/cast-{name}-fixture"),
        "{name}: fixture homepage is part of the package metadata golden"
    );
    assert_eq!(
        planned
            .plan
            .package
            .licenses
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["MPL-2.0"]
    );
    assert!(planned.plan.analysis.debug, "{name}: fixtures exercise debug splitting");
    assert!(
        planned.plan.analysis.strip,
        "{name}: fixtures exercise deterministic stripping"
    );
    assert!(
        !planned.plan.analysis.compress_man,
        "{name}: the tracked manual-page bytes require compression to be explicitly disabled"
    );

    let expected_names = planned
        .plan
        .outputs
        .iter()
        .map(|output| package_filename(planned, output))
        .chain([
            format!("manifest.{}.bin", planned.plan.package.architecture),
            format!("manifest.{}.jsonc", planned.plan.package.architecture),
        ])
        .collect::<BTreeSet<_>>();
    let artefacts = BundleDirectory::open(name, root, role).snapshot(name, &expected_names);

    let output_names = planned
        .plan
        .outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect::<BTreeSet<_>>();
    if name == "split" {
        assert_eq!(
            output_names,
            BTreeSet::from(["out", "libs", "devel", "docs", "dbginfo"])
        );
    } else {
        assert_eq!(
            output_names,
            BTreeSet::from([
                "out",
                "docs",
                "devel",
                "dbginfo",
                "libs",
                "32bit",
                "32bit-devel",
                "32bit-dbginfo",
                "demos",
            ]),
            "{name}: the versioned package factory output ABI drifted"
        );
    }

    let mut packages = BTreeMap::new();
    for output in &planned.plan.outputs {
        let filename = package_filename(planned, output);
        let image = decode_package(name, planned, output, &artefacts[&filename]);
        assert!(
            packages.insert(output.package_name.clone(), image).is_none(),
            "{name}: duplicate emitted package name {}",
            output.package_name
        );
    }

    assert_global_layout_integrity(name, &packages);
    assert_manifests(name, planned, &artefacts, &packages);
    if name == "split" {
        assert_split_fixture(planned, &packages);
    } else {
        assert_simple_fixture(name, planned, &packages);
    }
}

fn fixture_limits() -> StoneDecodeLimits {
    StoneDecodeLimits {
        max_payloads: 16,
        max_records_per_payload: 4_096,
        max_record_bytes: 64 * 1024,
        max_stored_payload_bytes: MAX_FIXTURE_CONTENT_BYTES,
        max_plain_payload_bytes: MAX_FIXTURE_CONTENT_BYTES,
        max_total_records: 16_384,
        max_total_record_bytes: 8 * 1024 * 1024,
        max_total_stored_bytes: MAX_FIXTURE_ARTEFACT_BYTES,
        max_total_plain_bytes: MAX_FIXTURE_ARTEFACT_BYTES,
        max_zstd_window_log: 24,
    }
}

fn package_filename(planned: &super::super::Planned, output: &OutputPlan) -> String {
    format!(
        "{}-{}-{}-{}-{}.stone",
        output.package_name,
        planned.plan.package.version,
        planned.plan.package.source_release,
        planned.plan.package.build_release,
        planned.plan.package.architecture,
    )
}

fn decode_package(fixture: &str, planned: &super::super::Planned, output: &OutputPlan, bytes: &[u8]) -> PackageImage {
    let mut reader = stone::read_bytes_with_limits(bytes, fixture_limits())
        .unwrap_or_else(|error| panic!("{fixture}: decode emitted package {}: {error}", output.package_name));
    let StoneHeader::V1(container_header) = reader.header;
    assert_eq!(
        container_header.file_type,
        StoneHeaderV1FileType::Binary,
        "{fixture}: {} has the wrong Stone file type",
        output.package_name
    );
    let payloads = reader
        .payloads()
        .unwrap_or_else(|error| panic!("{fixture}: seek package payloads for {}: {error}", output.package_name))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| {
            panic!(
                "{fixture}: decode package payloads for {}: {error}",
                output.package_name
            )
        });
    assert_eq!(
        usize::from(container_header.num_payloads),
        payloads.len(),
        "{fixture}: {} Stone header payload cardinality drift",
        output.package_name
    );
    assert_canonical_payload_headers(fixture, &output.package_name, &payloads);

    let payload_names = payloads.iter().map(StoneDecodedPayload::name).collect::<Vec<_>>();
    let metadata = payloads
        .iter()
        .filter_map(StoneDecodedPayload::meta)
        .collect::<Vec<_>>();
    assert_eq!(
        metadata.len(),
        1,
        "{fixture}: {} must have exactly one Meta payload",
        output.package_name
    );
    let records = metadata[0].body.clone();
    validate_meta_records(fixture, &output.package_name, &records, false);
    assert_frozen_provenance(fixture, planned, &output.package_name, &records);
    let meta = Meta::from_stone_payload(&records)
        .unwrap_or_else(|error| panic!("{fixture}: decode metadata for {}: {error}", output.package_name));
    assert_package_meta(fixture, planned, output, &records, &meta);

    let layout_payloads = payloads
        .iter()
        .filter_map(StoneDecodedPayload::layout)
        .collect::<Vec<_>>();
    assert!(
        layout_payloads.len() <= 1,
        "{fixture}: {} repeats its Layout payload",
        output.package_name
    );
    let layout_records = layout_payloads
        .first()
        .map(|payload| {
            assert_eq!(payload.header.num_records, payload.body.len());
            payload.body.clone()
        })
        .unwrap_or_default();
    assert_eq!(
        layout_payloads.is_empty(),
        layout_records.is_empty(),
        "{fixture}: {} encoded an empty Layout payload",
        output.package_name
    );

    let mut layouts = BTreeMap::new();
    let mut regular_digests = BTreeSet::new();
    for layout in layout_records {
        validate_layout_record(fixture, &output.package_name, &layout);
        let target = layout.file.target().to_owned();
        if let StonePayloadLayoutFile::Regular(digest, _) = &layout.file {
            regular_digests.insert(*digest);
        }
        assert!(
            layouts.insert(target.clone(), layout).is_none(),
            "{fixture}: {} repeats layout target /usr/{target}",
            output.package_name
        );
    }

    let index_payloads = payloads
        .iter()
        .filter_map(StoneDecodedPayload::index)
        .collect::<Vec<_>>();
    let content_payloads = payloads
        .iter()
        .filter_map(StoneDecodedPayload::content)
        .collect::<Vec<_>>();
    assert!(
        index_payloads.len() <= 1,
        "{fixture}: {} repeats its Index payload",
        output.package_name
    );
    assert!(
        content_payloads.len() <= 1,
        "{fixture}: {} repeats its Content payload",
        output.package_name
    );

    if regular_digests.is_empty() {
        assert!(
            index_payloads.is_empty(),
            "{fixture}: {} has an orphan Index payload",
            output.package_name
        );
        assert!(
            content_payloads.is_empty(),
            "{fixture}: {} has an orphan Content payload",
            output.package_name
        );
        assert_eq!(
            payload_names,
            if layouts.is_empty() {
                vec!["Meta"]
            } else {
                vec!["Meta", "Layout"]
            },
            "{fixture}: {} has a non-canonical payload topology",
            output.package_name
        );
        return PackageImage {
            records,
            meta,
            layouts,
            content: BTreeMap::new(),
        };
    }

    assert_eq!(
        payload_names,
        ["Meta", "Layout", "Index", "Content"],
        "{fixture}: {} has a non-canonical content payload topology",
        output.package_name
    );
    let indices = &index_payloads[0].body;
    assert_eq!(index_payloads[0].header.num_records, indices.len());
    assert_eq!(
        indices.len(),
        regular_digests.len(),
        "{fixture}: {} must index each unique regular-file digest exactly once",
        output.package_name
    );

    let content_payload = content_payloads[0].clone();
    assert!(
        content_payload.header.plain_size <= MAX_FIXTURE_CONTENT_BYTES,
        "{fixture}: {} content exceeds the fixture boundary",
        output.package_name
    );
    let mut unpacked = Vec::new();
    unpacked
        .try_reserve_exact(usize::try_from(content_payload.header.plain_size).unwrap())
        .unwrap_or_else(|error| panic!("{fixture}: reserve content for {}: {error}", output.package_name));
    reader
        .unpack_content(&content_payload, &mut unpacked)
        .unwrap_or_else(|error| panic!("{fixture}: unpack content for {}: {error}", output.package_name));
    assert_eq!(
        u64::try_from(unpacked.len()).unwrap(),
        content_payload.header.plain_size
    );

    let mut content = BTreeMap::new();
    let mut cursor = 0u64;
    for index in indices {
        assert_eq!(
            index.start, cursor,
            "{fixture}: {} index ranges must be gapless and ordered",
            output.package_name
        );
        assert!(
            index.end >= index.start,
            "{fixture}: {} has a reversed index range",
            output.package_name
        );
        let start = usize::try_from(index.start).unwrap();
        let end = usize::try_from(index.end).unwrap();
        let blob = unpacked
            .get(start..end)
            .unwrap_or_else(|| panic!("{fixture}: {} index range escapes Content", output.package_name));
        let digest = content_digest(blob);
        assert_eq!(
            digest, index.digest,
            "{fixture}: {} index XXH3 digest does not authenticate its content range",
            output.package_name
        );
        assert!(
            content.insert(index.digest, blob.to_vec()).is_none(),
            "{fixture}: {} repeats an Index digest",
            output.package_name
        );
        cursor = index.end;
    }
    assert_eq!(
        cursor,
        u64::try_from(unpacked.len()).unwrap(),
        "{fixture}: {} Content has unindexed trailing bytes",
        output.package_name
    );
    assert_eq!(
        content.keys().copied().collect::<BTreeSet<_>>(),
        regular_digests,
        "{fixture}: {} Layout and Index digest sets disagree",
        output.package_name
    );

    PackageImage {
        records,
        meta,
        layouts,
        content,
    }
}

fn content_digest(bytes: &[u8]) -> u128 {
    let mut hasher = StoneDigestWriterHasher::new();
    hasher.update(bytes);
    hasher.digest128()
}

fn assert_canonical_payload_headers(fixture: &str, role: &str, payloads: &[StoneDecodedPayload]) {
    for payload in payloads {
        let header = payload.header();
        let expected_kind = match payload {
            StoneDecodedPayload::Meta(_) => StonePayloadKind::Meta,
            StoneDecodedPayload::Attributes(_) => StonePayloadKind::Attributes,
            StoneDecodedPayload::Layout(_) => StonePayloadKind::Layout,
            StoneDecodedPayload::Index(_) => StonePayloadKind::Index,
            StoneDecodedPayload::Content(_) => StonePayloadKind::Content,
            StoneDecodedPayload::Unknown(_) | StoneDecodedPayload::UnknownCompression(_) => {
                panic!("{fixture}: {role} contains an unknown Stone payload")
            }
        };
        assert_eq!(header.version, 1, "{fixture}: {role} contains a non-v1 Stone payload");
        assert_eq!(
            header.compression,
            StonePayloadCompression::Zstd,
            "{fixture}: {role} payload {:?} is not canonically zstd-compressed",
            header.kind
        );
        assert_eq!(
            header.kind, expected_kind,
            "{fixture}: {role} decoded payload variant disagrees with its header"
        );
        assert!(
            header.stored_size > 0,
            "{fixture}: {role} contains an empty stored payload"
        );
        assert!(
            header.plain_size > 0,
            "{fixture}: {role} contains an empty plain payload"
        );
        if expected_kind == StonePayloadKind::Content {
            assert_eq!(
                header.num_records, 0,
                "{fixture}: {role} Content must use the canonical zero record count"
            );
        } else {
            assert!(
                header.num_records > 0,
                "{fixture}: {role} contains a canonically forbidden empty record payload"
            );
        }
    }
}

fn validate_meta_records(fixture: &str, package: &str, records: &[StonePayloadMetaRecord], manifest: bool) {
    let fixed_tags = [
        StonePayloadMetaTag::Name,
        StonePayloadMetaTag::Version,
        StonePayloadMetaTag::Release,
        StonePayloadMetaTag::BuildRelease,
        StonePayloadMetaTag::Architecture,
        StonePayloadMetaTag::Summary,
        StonePayloadMetaTag::Description,
        StonePayloadMetaTag::SourceID,
        StonePayloadMetaTag::Homepage,
    ];
    assert!(
        records.len() >= fixed_tags.len() + 3,
        "{fixture}: {package} metadata is shorter than the canonical fixed fields, license, and provenance"
    );
    assert_eq!(
        records[..fixed_tags.len()]
            .iter()
            .map(|record| record.tag)
            .collect::<Vec<_>>(),
        fixed_tags,
        "{fixture}: {package} fixed metadata record order drift"
    );

    let mut unique = BTreeSet::new();
    for record in records {
        assert!(
            unique.insert(format!("{:?}:{:?}", record.tag, record.primitive)),
            "{fixture}: {package} repeats metadata record {:?}",
            record.tag
        );
        match record.tag {
            StonePayloadMetaTag::Name
            | StonePayloadMetaTag::Architecture
            | StonePayloadMetaTag::Version
            | StonePayloadMetaTag::Summary
            | StonePayloadMetaTag::Description
            | StonePayloadMetaTag::Homepage
            | StonePayloadMetaTag::SourceID
            | StonePayloadMetaTag::License
            | StonePayloadMetaTag::SourceRef => {
                let StonePayloadMetaPrimitive::String(value) = &record.primitive else {
                    panic!(
                        "{fixture}: {package} metadata tag {:?} has the wrong primitive",
                        record.tag
                    );
                };
                assert!(!value.contains('\0'), "{fixture}: {package} metadata contains NUL");
            }
            StonePayloadMetaTag::Release | StonePayloadMetaTag::BuildRelease => assert!(
                matches!(&record.primitive, StonePayloadMetaPrimitive::Uint64(_)),
                "{fixture}: {package} metadata tag {:?} has the wrong primitive",
                record.tag
            ),
            StonePayloadMetaTag::Depends | StonePayloadMetaTag::BuildDepends => {
                assert!(
                    record.tag != StonePayloadMetaTag::BuildDepends || manifest,
                    "{fixture}: binary package {package} contains manifest-only BuildDepends"
                );
                let StonePayloadMetaPrimitive::Dependency(kind, value) = &record.primitive else {
                    panic!(
                        "{fixture}: {package} metadata tag {:?} has the wrong primitive",
                        record.tag
                    );
                };
                let kind = Kind::from_stone_dependency(*kind)
                    .unwrap_or_else(|| panic!("{fixture}: {package} uses an unknown dependency kind"));
                Dependency::new(kind, value.clone())
                    .unwrap_or_else(|error| panic!("{fixture}: {package} has an invalid dependency: {error}"));
            }
            StonePayloadMetaTag::Provides | StonePayloadMetaTag::Conflicts => {
                let StonePayloadMetaPrimitive::Provider(kind, value) = &record.primitive else {
                    panic!(
                        "{fixture}: {package} metadata tag {:?} has the wrong primitive",
                        record.tag
                    );
                };
                let kind = Kind::from_stone_dependency(*kind)
                    .unwrap_or_else(|| panic!("{fixture}: {package} uses an unknown provider kind"));
                Provider::new(kind, value.clone())
                    .unwrap_or_else(|error| panic!("{fixture}: {package} has an invalid provider: {error}"));
            }
            StonePayloadMetaTag::PackageURI
            | StonePayloadMetaTag::PackageHash
            | StonePayloadMetaTag::PackageSize
            | StonePayloadMetaTag::SourceURI
            | StonePayloadMetaTag::SourcePath
            | StonePayloadMetaTag::Unknown => {
                panic!("{fixture}: {package} contains forbidden metadata tag {:?}", record.tag)
            }
        }
        assert!(
            !matches!(&record.primitive, StonePayloadMetaPrimitive::Unknown(_)),
            "{fixture}: {package} contains an unknown metadata primitive"
        );
    }

    for tag in [
        StonePayloadMetaTag::Name,
        StonePayloadMetaTag::Architecture,
        StonePayloadMetaTag::Version,
        StonePayloadMetaTag::Summary,
        StonePayloadMetaTag::Description,
        StonePayloadMetaTag::Homepage,
        StonePayloadMetaTag::SourceID,
        StonePayloadMetaTag::Release,
        StonePayloadMetaTag::BuildRelease,
    ] {
        assert_eq!(
            records.iter().filter(|record| record.tag == tag).count(),
            1,
            "{fixture}: {package} must contain exactly one {tag:?} metadata record"
        );
    }
    assert_eq!(
        records
            .iter()
            .filter(|record| record.tag == StonePayloadMetaTag::SourceRef)
            .count(),
        2,
        "{fixture}: {package} must contain exactly two frozen provenance references"
    );

    let variable_ranks = records[fixed_tags.len()..]
        .iter()
        .map(|record| match record.tag {
            StonePayloadMetaTag::License => 0,
            StonePayloadMetaTag::Depends => 1,
            StonePayloadMetaTag::Provides => 2,
            StonePayloadMetaTag::Conflicts => 3,
            StonePayloadMetaTag::SourceRef => 4,
            StonePayloadMetaTag::BuildDepends if manifest => 5,
            tag => panic!("{fixture}: {package} metadata tag {tag:?} is outside canonical record order"),
        })
        .collect::<Vec<_>>();
    assert!(
        variable_ranks.windows(2).all(|pair| pair[0] <= pair[1]),
        "{fixture}: {package} variable metadata record groups are not canonical"
    );
    for tag in [
        StonePayloadMetaTag::License,
        StonePayloadMetaTag::Depends,
        StonePayloadMetaTag::Provides,
        StonePayloadMetaTag::Conflicts,
        StonePayloadMetaTag::BuildDepends,
    ] {
        let values = records
            .iter()
            .filter(|record| record.tag == tag)
            .map(|record| format!("{:?}", record.primitive))
            .collect::<Vec<_>>();
        assert!(
            values.windows(2).all(|pair| pair[0] < pair[1]),
            "{fixture}: {package} {tag:?} records are not strictly canonical"
        );
    }
}

fn assert_frozen_provenance(
    fixture: &str,
    planned: &super::super::Planned,
    package: &str,
    records: &[StonePayloadMetaRecord],
) {
    let actual = records
        .iter()
        .filter_map(|record| match (&record.tag, &record.primitive) {
            (StonePayloadMetaTag::SourceRef, StonePayloadMetaPrimitive::String(value)) => Some(value.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let recipe = format!("gluon-evaluation-sha256:{}", planned.plan.provenance.recipe.sha256);
    let derivation = format!("derivation-sha256:{}", planned.plan.derivation_id());
    assert_eq!(
        actual,
        BTreeSet::from([recipe.as_str(), derivation.as_str()]),
        "{fixture}: {package} provenance does not name the exact recipe and derivation"
    );
}

fn assert_package_meta(
    fixture: &str,
    planned: &super::super::Planned,
    output: &OutputPlan,
    records: &[StonePayloadMetaRecord],
    meta: &Meta,
) {
    assert_eq!(meta.name.as_str(), output.package_name);
    assert_eq!(meta.version_identifier, planned.plan.package.version);
    assert_eq!(meta.source_release, planned.plan.package.source_release);
    assert_eq!(meta.build_release, planned.plan.package.build_release);
    assert_eq!(meta.architecture, planned.plan.package.architecture);
    assert_eq!(meta.summary, output.summary.as_deref().unwrap_or_default());
    assert_eq!(meta.description, output.description.as_deref().unwrap_or_default());
    assert_eq!(meta.source_id, planned.plan.package.name);
    assert_eq!(meta.homepage, planned.plan.package.homepage);
    let mut licenses = planned.plan.package.licenses.clone();
    licenses.sort();
    assert_eq!(meta.licenses, licenses);
    assert_eq!(meta.uri, None);
    assert_eq!(meta.hash, None);
    assert_eq!(meta.download_size, None);
    assert_eq!(
        meta.conflicts,
        output.conflicts.iter().map(|relation| relation.to_provider()).collect(),
        "{fixture}: {} conflicts drifted from the frozen output",
        output.package_name
    );

    let raw_dependencies = raw_dependencies(records, StonePayloadMetaTag::Depends);
    assert_eq!(
        raw_dependencies,
        meta.dependencies.iter().map(Dependency::to_name).collect(),
        "{fixture}: {} dependency records are not canonical",
        output.package_name
    );
    let mut decoded_providers = meta.providers.iter().map(Provider::to_name).collect::<BTreeSet<_>>();
    decoded_providers.remove(&output.package_name);
    assert_eq!(
        raw_providers(records, StonePayloadMetaTag::Provides),
        decoded_providers,
        "{fixture}: {} provider records are not canonical",
        output.package_name
    );
    assert_eq!(
        raw_providers(records, StonePayloadMetaTag::Conflicts),
        meta.conflicts.iter().map(Provider::to_name).collect(),
        "{fixture}: {} conflict records are not canonical",
        output.package_name
    );

    for relation in &output.runtime_inputs {
        let expected = match relation {
            OutputRelation::Locked { relation, .. } => relation.canonical_name(),
            OutputRelation::Planned {
                output: dependency_output,
            } => planned
                .plan
                .outputs
                .iter()
                .find(|candidate| candidate.name == *dependency_output)
                .unwrap_or_else(|| panic!("{fixture}: missing planned output dependency {dependency_output}"))
                .package_name
                .clone(),
        };
        assert!(
            raw_dependencies.contains(&expected),
            "{fixture}: {} omits frozen runtime relation {expected}",
            output.package_name
        );
    }
}

fn raw_dependencies(records: &[StonePayloadMetaRecord], tag: StonePayloadMetaTag) -> BTreeSet<String> {
    records
        .iter()
        .filter_map(|record| {
            if record.tag != tag {
                return None;
            }
            let StonePayloadMetaPrimitive::Dependency(kind, name) = &record.primitive else {
                unreachable!("metadata primitive was checked before relation extraction")
            };
            let kind = Kind::from_stone_dependency(*kind).expect("relation kind was checked");
            Some(Dependency::new(kind, name.clone()).unwrap().to_name())
        })
        .collect()
}

fn raw_providers(records: &[StonePayloadMetaRecord], tag: StonePayloadMetaTag) -> BTreeSet<String> {
    records
        .iter()
        .filter_map(|record| {
            if record.tag != tag {
                return None;
            }
            let StonePayloadMetaPrimitive::Provider(kind, name) = &record.primitive else {
                unreachable!("metadata primitive was checked before relation extraction")
            };
            let kind = Kind::from_stone_dependency(*kind).expect("relation kind was checked");
            Some(Provider::new(kind, name.clone()).unwrap().to_name())
        })
        .collect()
}

fn validate_layout_record(fixture: &str, package: &str, layout: &StonePayloadLayoutRecord) {
    assert_eq!(layout.uid, 0, "{fixture}: {package} layout owner must be root");
    assert_eq!(layout.gid, 0, "{fixture}: {package} layout group must be root");
    assert_eq!(layout.tag, 0, "{fixture}: {package} uses an unsupported layout tag");
    let target = layout.file.target();
    validate_target_path(fixture, package, target);

    let (file_type, expected_permissions) = match &layout.file {
        StonePayloadLayoutFile::Regular(_, _) => (nix::libc::S_IFREG, [0o644, 0o755].as_slice()),
        StonePayloadLayoutFile::Symlink(source, _) => {
            validate_symlink_source(fixture, package, target, source);
            (nix::libc::S_IFLNK, [0o777].as_slice())
        }
        StonePayloadLayoutFile::Directory(_) => (nix::libc::S_IFDIR, [0o755].as_slice()),
        StonePayloadLayoutFile::CharacterDevice(_)
        | StonePayloadLayoutFile::BlockDevice(_)
        | StonePayloadLayoutFile::Fifo(_)
        | StonePayloadLayoutFile::Socket(_)
        | StonePayloadLayoutFile::Unknown(_, _) => {
            panic!("{fixture}: {package} emits unsupported special layout /usr/{target}")
        }
    };
    assert_eq!(
        layout.mode & nix::libc::S_IFMT,
        file_type,
        "{fixture}: {package} layout type and mode disagree for /usr/{target}"
    );
    assert_eq!(
        layout.mode & !(nix::libc::S_IFMT | 0o7777),
        0,
        "{fixture}: {package} layout has unsupported mode bits for /usr/{target}"
    );
    assert_eq!(
        layout.mode & 0o7000,
        0,
        "{fixture}: {package} layout must not carry setuid, setgid, or sticky bits"
    );
    assert!(
        expected_permissions.contains(&(layout.mode & 0o777)),
        "{fixture}: {package} layout has unexpected permissions {:o} for /usr/{target}",
        layout.mode & 0o777
    );
}

fn validate_target_path(fixture: &str, package: &str, target: &str) {
    assert!(!target.is_empty(), "{fixture}: {package} has an empty layout target");
    assert!(target.len() <= 4_096, "{fixture}: {package} layout target is too long");
    assert!(
        !target.starts_with('/'),
        "{fixture}: {package} layout target must be /usr-relative"
    );
    assert!(
        !target.ends_with('/'),
        "{fixture}: {package} layout target has a trailing separator"
    );
    assert!(
        !target.bytes().any(|byte| byte == 0 || byte.is_ascii_control()),
        "{fixture}: {package} layout target contains a control byte"
    );
    let components = Path::new(target)
        .components()
        .map(|component| match component {
            Component::Normal(component) => component
                .to_str()
                .unwrap_or_else(|| panic!("{fixture}: {package} layout target is not UTF-8")),
            _ => panic!("{fixture}: {package} layout target is not a normalized relative path: {target:?}"),
        })
        .collect::<Vec<_>>();
    assert!(!components.is_empty());
    assert!(components.len() <= 64, "{fixture}: {package} layout target is too deep");
    assert_eq!(
        components.join("/"),
        target,
        "{fixture}: {package} layout target is not canonical"
    );
}

fn validate_symlink_source(fixture: &str, package: &str, target: &str, source: &str) {
    assert!(
        !source.is_empty(),
        "{fixture}: {package} symlink /usr/{target} has an empty source"
    );
    assert!(source.len() <= 4_096, "{fixture}: {package} symlink source is too long");
    assert!(
        !source.bytes().any(|byte| byte == 0 || byte.is_ascii_control()),
        "{fixture}: {package} symlink source contains a control byte"
    );
    assert!(
        !Path::new(source).is_absolute(),
        "{fixture}: {package} fixture symlink /usr/{target} must use a relative source"
    );
    let _ = resolve_symlink_target(fixture, package, target, source);
}

fn resolve_symlink_target(fixture: &str, package: &str, target: &str, source: &str) -> String {
    let mut resolved = Path::new(target)
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for component in Path::new(source).components() {
        match component {
            Component::Normal(value) => resolved.push(value.to_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                assert!(
                    resolved.pop().is_some(),
                    "{fixture}: {package} symlink /usr/{target} escapes /usr"
                );
            }
            Component::RootDir | Component::Prefix(_) => {
                panic!("{fixture}: {package} symlink /usr/{target} is not relative")
            }
        }
    }
    let resolved = resolved
        .iter()
        .map(|component| component.to_str().expect("validated UTF-8 symlink component"))
        .collect::<Vec<_>>()
        .join("/");
    validate_target_path(fixture, package, &resolved);
    resolved
}

fn assert_global_layout_integrity(fixture: &str, packages: &BTreeMap<String, PackageImage>) {
    let mut global = BTreeMap::<String, (&str, &StonePayloadLayoutRecord)>::new();
    for (package, image) in packages {
        for (target, layout) in &image.layouts {
            assert!(
                global.insert(target.clone(), (package, layout)).is_none(),
                "{fixture}: /usr/{target} is emitted by more than one output"
            );
        }
    }

    for (target, (package, layout)) in &global {
        let mut ancestor = Path::new(target).parent();
        while let Some(path) = ancestor {
            if path.as_os_str().is_empty() {
                break;
            }
            let path = path.to_str().expect("validated UTF-8 layout path");
            if let Some((ancestor_package, ancestor_layout)) = global.get(path) {
                assert!(
                    matches!(&ancestor_layout.file, StonePayloadLayoutFile::Directory(_)),
                    "{fixture}: terminal /usr/{path} from {ancestor_package} is an ancestor of /usr/{target} from {package}"
                );
            }
            ancestor = Path::new(path).parent();
        }

        if let StonePayloadLayoutFile::Symlink(source, _) = &layout.file {
            let mut resolved = resolve_symlink_target(fixture, package, target, source);
            let mut visited = BTreeSet::from([target.clone()]);
            loop {
                assert!(
                    visited.insert(resolved.clone()),
                    "{fixture}: package symlink cycle reaches /usr/{resolved}"
                );
                let (resolved_package, resolved_layout) = global.get(&resolved).unwrap_or_else(|| {
                    panic!(
                        "{fixture}: {package} symlink /usr/{target} resolves to missing package path /usr/{resolved}"
                    )
                });
                match &resolved_layout.file {
                    StonePayloadLayoutFile::Symlink(next, _) => {
                        resolved = resolve_symlink_target(fixture, resolved_package, &resolved, next);
                    }
                    StonePayloadLayoutFile::Regular(_, _) | StonePayloadLayoutFile::Directory(_) => break,
                    _ => panic!("{fixture}: symlink /usr/{target} resolves to an unsupported inode"),
                }
            }
        }
    }
}

fn assert_manifests(
    fixture: &str,
    planned: &super::super::Planned,
    artefacts: &BTreeMap<String, Vec<u8>>,
    packages: &BTreeMap<String, PackageImage>,
) {
    let included = planned
        .plan
        .outputs
        .iter()
        .filter(|output| output.include_in_manifest)
        .map(|output| output.package_name.as_str())
        .collect::<BTreeSet<_>>();
    let expected_build_dependencies = planned
        .plan
        .manifest_build_inputs
        .iter()
        .map(|relation| relation.canonical_name())
        .collect::<BTreeSet<_>>();

    let binary_name = format!("manifest.{}.bin", planned.plan.package.architecture);
    let binary = &artefacts[&binary_name];
    let mut reader = stone::read_bytes_with_limits(binary, fixture_limits())
        .unwrap_or_else(|error| panic!("{fixture}: decode binary build manifest: {error}"));
    let StoneHeader::V1(container_header) = reader.header;
    assert_eq!(
        container_header.file_type,
        StoneHeaderV1FileType::BuildManifest,
        "{fixture}: binary build manifest has the wrong Stone file type"
    );
    let payloads = reader
        .payloads()
        .unwrap_or_else(|error| panic!("{fixture}: seek binary manifest payloads: {error}"))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("{fixture}: decode binary manifest payloads: {error}"));
    assert_eq!(payloads.len(), included.len());
    assert_eq!(usize::from(container_header.num_payloads), included.len());
    assert_canonical_payload_headers(fixture, "binary build manifest", &payloads);
    assert!(
        payloads
            .iter()
            .all(|payload| matches!(payload, StoneDecodedPayload::Meta(_))),
        "{fixture}: binary build manifest may contain only Meta payloads"
    );
    let mut manifested = BTreeSet::new();
    let mut manifest_order = Vec::new();
    for payload in &payloads {
        let records = &payload.meta().expect("manifest payload was checked").body;
        let decoded = Meta::from_stone_payload(records)
            .unwrap_or_else(|error| panic!("{fixture}: decode binary manifest package metadata: {error}"));
        let package = decoded.name.to_string();
        validate_meta_records(fixture, &package, records, true);
        assert_frozen_provenance(fixture, planned, &package, records);
        assert!(
            manifested.insert(package.clone()),
            "{fixture}: binary manifest repeats {package}"
        );
        manifest_order.push(package.clone());
        let image = packages
            .get(&package)
            .unwrap_or_else(|| panic!("{fixture}: binary manifest names unknown output {package}"));
        let mut expected = image.meta.clone();
        expected.build_release = 0;
        // Forge's legacy Meta view intentionally flattens both Depends and
        // BuildDepends primitives. Keep checking the raw tags separately
        // below, then model that lossy view for the whole-struct comparison.
        expected.dependencies.extend(
            planned
                .plan
                .manifest_build_inputs
                .iter()
                .map(|relation| relation.to_dependency()),
        );
        assert_eq!(
            decoded, expected,
            "{fixture}: binary manifest metadata drift for {package}"
        );
        assert_eq!(
            raw_dependencies(records, StonePayloadMetaTag::BuildDepends),
            expected_build_dependencies,
            "{fixture}: binary manifest build closure drift for {package}"
        );
    }
    assert_eq!(manifested.iter().map(String::as_str).collect::<BTreeSet<_>>(), included);
    assert_eq!(
        manifest_order,
        included.iter().map(|package| (*package).to_owned()).collect::<Vec<_>>(),
        "{fixture}: binary manifest package payload order is not canonical"
    );

    let json_name = format!("manifest.{}.jsonc", planned.plan.package.architecture);
    let jsonc = std::str::from_utf8(&artefacts[&json_name])
        .unwrap_or_else(|error| panic!("{fixture}: JSONC build report is not UTF-8: {error}"));
    let (comment, json) = jsonc
        .split_once('\n')
        .unwrap_or_else(|| panic!("{fixture}: JSONC report has no comment boundary"));
    assert_eq!(comment, "/** Human readable report. This is not consumed by Cast */");
    let report: serde_json::Value =
        serde_json::from_str(json).unwrap_or_else(|error| panic!("{fixture}: decode JSONC build report: {error}"));
    let report_object = report.as_object().expect("JSONC report must be an object");
    assert_eq!(
        report_object.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "manifest-version",
            "packages",
            "derivation-id",
            "recipe-fingerprint",
            "source-name",
            "source-release",
            "source-version",
        ]),
        "{fixture}: JSONC report schema drift"
    );
    assert_eq!(report["manifest-version"], "0.2");
    assert_eq!(report["derivation-id"], planned.plan.derivation_id().as_str());
    assert_eq!(report["recipe-fingerprint"], planned.plan.provenance.recipe.sha256);
    assert_eq!(report["source-name"], planned.plan.package.name);
    assert_eq!(
        report["source-release"],
        planned.plan.package.source_release.to_string()
    );
    assert_eq!(report["source-version"], planned.plan.package.version);

    let report_packages = report["packages"]
        .as_object()
        .expect("JSONC packages must be an object");
    assert_eq!(
        report_packages.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        included,
        "{fixture}: binary and JSONC manifest membership disagree"
    );
    for package in included {
        let image = &packages[package];
        let report_package = report_packages
            .get(package)
            .expect("manifest membership was checked")
            .as_object()
            .unwrap_or_else(|| panic!("{fixture}: JSONC package {package} is not an object"));
        assert!(
            report_package.keys().all(|key| matches!(
                key.as_str(),
                "name" | "files" | "depends" | "provides" | "build-depends"
            )),
            "{fixture}: JSONC package {package} contains an unknown field"
        );
        assert_eq!(
            report_package.get("name").and_then(serde_json::Value::as_str),
            Some(package)
        );

        let files = image
            .layouts
            .keys()
            .map(|target| format!("/usr/{target}"))
            .collect::<Vec<_>>();
        let dependencies = raw_dependencies(&image.records, StonePayloadMetaTag::Depends)
            .into_iter()
            .collect::<Vec<_>>();
        let providers = raw_providers(&image.records, StonePayloadMetaTag::Provides)
            .into_iter()
            .collect::<Vec<_>>();
        assert_json_string_array(fixture, package, report_package, "files", &files);
        assert_json_string_array(fixture, package, report_package, "depends", &dependencies);
        assert_json_string_array(fixture, package, report_package, "provides", &providers);
        assert_json_string_array(
            fixture,
            package,
            report_package,
            "build-depends",
            &expected_build_dependencies.iter().cloned().collect::<Vec<_>>(),
        );
    }
}

fn assert_json_string_array(
    fixture: &str,
    package: &str,
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    expected: &[String],
) {
    let actual = object
        .get(field)
        .map(|value| {
            value
                .as_array()
                .unwrap_or_else(|| panic!("{fixture}: JSONC {package}.{field} is not an array"))
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .unwrap_or_else(|| panic!("{fixture}: JSONC {package}.{field} contains a non-string"))
                        .to_owned()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert_eq!(actual, expected, "{fixture}: JSONC {package}.{field} drift");
    assert_eq!(
        object.contains_key(field),
        !expected.is_empty(),
        "{fixture}: JSONC {package}.{field} must be omitted exactly when empty"
    );
}

fn output<'a>(
    planned: &'a super::super::Planned,
    packages: &'a BTreeMap<String, PackageImage>,
    logical_name: &str,
) -> (&'a OutputPlan, &'a PackageImage) {
    let output = planned
        .plan
        .outputs
        .iter()
        .find(|output| output.name == logical_name)
        .unwrap_or_else(|| panic!("missing frozen output {logical_name}"));
    (output, &packages[&output.package_name])
}

fn planned_output_dependencies(planned: &super::super::Planned, output: &OutputPlan) -> BTreeSet<String> {
    output
        .runtime_inputs
        .iter()
        .map(|relation| match relation {
            OutputRelation::Locked { relation, .. } => relation.canonical_name(),
            OutputRelation::Planned { output } => planned
                .plan
                .outputs
                .iter()
                .find(|candidate| candidate.name == *output)
                .unwrap_or_else(|| panic!("missing planned runtime output {output}"))
                .package_name
                .clone(),
        })
        .collect()
}

fn assert_exact_relations(
    fixture: &str,
    image: &PackageImage,
    dependencies: BTreeSet<String>,
    providers: BTreeSet<String>,
) {
    assert_eq!(
        image
            .meta
            .dependencies
            .iter()
            .map(Dependency::to_name)
            .collect::<BTreeSet<_>>(),
        dependencies,
        "{fixture}: {} dependency set is not the exact allowed fixture set",
        image.meta.name
    );
    assert_eq!(
        image
            .meta
            .providers
            .iter()
            .map(Provider::to_name)
            .collect::<BTreeSet<_>>(),
        providers,
        "{fixture}: {} provider set is not the exact allowed fixture set",
        image.meta.name
    );
}

fn assert_simple_fixture(fixture: &str, planned: &super::super::Planned, packages: &BTreeMap<String, PackageImage>) {
    for output in &planned.plan.outputs {
        assert_eq!(
            output.include_in_manifest,
            !matches!(output.name.as_str(), "dbginfo" | "32bit-dbginfo"),
            "{fixture}: default manifest membership drift for {}",
            output.name
        );
    }

    let (root_plan, root) = output(planned, packages, "out");
    if fixture == "custom" {
        let target = "share/cast-custom-fixture/payload.txt";
        assert_leaf_paths(fixture, "out", root, [target]);
        assert_no_directories(fixture, "out", root);
        assert_regular(
            fixture,
            root,
            target,
            0o644,
            tracked_bytes("cast-custom-fixture-1.0.0", "payload.txt"),
        );
        assert_exact_relations(
            fixture,
            root,
            planned_output_dependencies(planned, root_plan),
            BTreeSet::from([root_plan.package_name.clone()]),
        );
        for candidate in &planned.plan.outputs {
            if candidate.name != "out" {
                assert!(
                    packages[&candidate.package_name].layouts.is_empty(),
                    "{fixture}: unexpected path in default output {}",
                    candidate.name
                );
                assert_exact_relations(
                    fixture,
                    &packages[&candidate.package_name],
                    planned_output_dependencies(planned, candidate),
                    BTreeSet::from([candidate.package_name.clone()]),
                );
            }
        }
        return;
    }

    let executable = format!("bin/cast-{fixture}-fixture");
    assert_leaf_paths(fixture, "out", root, [executable.as_str()]);
    assert_no_directories(fixture, "out", root);
    let bytes = regular_bytes(fixture, root, &executable);
    assert_eq!(root.layouts[&executable].mode & 0o777, 0o755);
    let executable_elf = assert_runtime_elf(fixture, &executable, bytes, RuntimeElfKind::Executable);
    let message = format!("cast {fixture} fixture");
    assert!(
        contains_bytes(bytes, message.as_bytes()),
        "{fixture}: installed executable does not contain its tracked fixture payload"
    );
    let mut root_dependencies = planned_output_dependencies(planned, root_plan);
    root_dependencies.extend(executable_elf.dependencies.iter().cloned());
    assert_exact_relations(
        fixture,
        root,
        root_dependencies,
        BTreeSet::from([
            root_plan.package_name.clone(),
            format!("binary(cast-{fixture}-fixture)"),
        ]),
    );

    let (debug_plan, debug) = output(planned, packages, "dbginfo");
    assert_debug_output(fixture, debug, &[executable_elf]);
    assert_exact_relations(
        fixture,
        debug,
        planned_output_dependencies(planned, debug_plan),
        BTreeSet::from([debug_plan.package_name.clone()]),
    );
    for candidate in &planned.plan.outputs {
        if !matches!(candidate.name.as_str(), "out" | "dbginfo") {
            assert!(
                packages[&candidate.package_name].layouts.is_empty(),
                "{fixture}: unexpected path in default output {}",
                candidate.name
            );
            assert_exact_relations(
                fixture,
                &packages[&candidate.package_name],
                planned_output_dependencies(planned, candidate),
                BTreeSet::from([candidate.package_name.clone()]),
            );
        }
    }
}

fn assert_split_fixture(planned: &super::super::Planned, packages: &BTreeMap<String, PackageImage>) {
    const FIXTURE: &str = "split";
    let flags = planned
        .plan
        .outputs
        .iter()
        .map(|output| (output.name.as_str(), output.include_in_manifest))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        flags,
        BTreeMap::from([
            ("out", true),
            ("libs", true),
            ("devel", true),
            ("docs", false),
            ("dbginfo", false)
        ])
    );
    assert!(
        planned
            .plan
            .collection_rules
            .iter()
            .any(|rule| { rule.output == "dbginfo" && rule.pattern == "/usr/lib/debug" }),
        "split: generated debug files have no explicit output rule"
    );

    let (root_plan, root) = output(planned, packages, "out");
    let (libs_plan, libs) = output(planned, packages, "libs");
    let (devel_plan, devel) = output(planned, packages, "devel");
    let (docs_plan, docs) = output(planned, packages, "docs");
    let (debug_plan, debug) = output(planned, packages, "dbginfo");
    assert_eq!(root_plan.package_name, "cast-split-fixture");
    assert_eq!(libs_plan.package_name, "cast-split-fixture-libs");
    assert_eq!(devel_plan.package_name, "cast-split-fixture-devel");
    assert_eq!(docs_plan.package_name, "cast-split-fixture-docs");
    assert_eq!(debug_plan.package_name, "cast-split-fixture-dbginfo");
    assert_eq!(
        planned
            .plan
            .outputs
            .iter()
            .map(|output| (output.name.as_str(), output.summary.as_deref()))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            ("out", Some("Split fixture executable and manual page")),
            ("libs", Some("Split fixture runtime library")),
            ("devel", Some("Split fixture development files")),
            ("docs", Some("Split fixture documentation")),
            ("dbginfo", Some("Split fixture debugging symbols")),
        ]),
        "split: output summaries are fixture metadata goldens"
    );
    assert!(
        planned.plan.outputs.iter().all(|output| output.description.is_none()),
        "split: fixture outputs must not grow implicit descriptions"
    );

    assert_leaf_paths(
        FIXTURE,
        "out",
        root,
        ["bin/cast-split-fixture", "share/man/man1/cast-split-fixture.1"],
    );
    assert_no_directories(FIXTURE, "out", root);
    assert_leaf_paths(
        FIXTURE,
        "libs",
        libs,
        ["lib/libcast-split.so.1", "lib/libcast-split.so.1.0.0"],
    );
    assert_no_directories(FIXTURE, "libs", libs);
    assert_leaf_paths(
        FIXTURE,
        "devel",
        devel,
        [
            "include/cast-split/libcastsplit.h",
            "lib/libcast-split.so",
            "lib/pkgconfig/cast-split.pc",
        ],
    );
    assert_no_directories(FIXTURE, "devel", devel);
    assert_leaf_paths(FIXTURE, "docs", docs, ["share/doc/cast-split-fixture/README.md"]);
    assert_no_directories(FIXTURE, "docs", docs);

    let executable = regular_bytes(FIXTURE, root, "bin/cast-split-fixture");
    assert_eq!(root.layouts["bin/cast-split-fixture"].mode & 0o777, 0o755);
    let executable_elf = assert_runtime_elf(
        FIXTURE,
        "bin/cast-split-fixture",
        executable,
        RuntimeElfKind::Executable,
    );
    assert_regular(
        FIXTURE,
        root,
        "share/man/man1/cast-split-fixture.1",
        0o644,
        tracked_bytes("cast-split-fixture-1.0.0", "cast-split-fixture.1"),
    );

    let library = regular_bytes(FIXTURE, libs, "lib/libcast-split.so.1.0.0");
    assert_eq!(libs.layouts["lib/libcast-split.so.1.0.0"].mode & 0o777, 0o755);
    let library_elf = assert_runtime_elf(
        FIXTURE,
        "lib/libcast-split.so.1.0.0",
        library,
        RuntimeElfKind::SharedLibrary,
    );
    assert!(
        contains_bytes(library, b"cast split fixture"),
        "split: shared library does not contain its tracked payload"
    );
    assert_symlink(FIXTURE, libs, "lib/libcast-split.so.1", "libcast-split.so.1.0.0");
    assert_symlink(FIXTURE, devel, "lib/libcast-split.so", "libcast-split.so.1");
    assert_regular(
        FIXTURE,
        devel,
        "include/cast-split/libcastsplit.h",
        0o644,
        tracked_bytes("cast-split-fixture-1.0.0", "libcastsplit.h"),
    );
    assert_regular(
        FIXTURE,
        docs,
        "share/doc/cast-split-fixture/README.md",
        0o644,
        tracked_bytes("cast-split-fixture-1.0.0", "README.md"),
    );
    assert_regular(
        FIXTURE,
        devel,
        "lib/pkgconfig/cast-split.pc",
        0o644,
        expected_split_pkgconfig(),
    );

    let mut root_dependencies = planned_output_dependencies(planned, root_plan);
    root_dependencies.extend(executable_elf.dependencies.iter().cloned());
    assert_exact_relations(
        FIXTURE,
        root,
        root_dependencies,
        BTreeSet::from([root_plan.package_name.clone(), "binary(cast-split-fixture)".to_owned()]),
    );
    let mut library_dependencies = planned_output_dependencies(planned, libs_plan);
    library_dependencies.extend(library_elf.dependencies.iter().cloned());
    let library_soname = library_elf
        .soname
        .as_deref()
        .expect("shared-library SONAME was structurally required");
    assert_exact_relations(
        FIXTURE,
        libs,
        library_dependencies,
        BTreeSet::from([
            libs_plan.package_name.clone(),
            format!("soname({library_soname}(x86_64))"),
        ]),
    );
    assert_exact_relations(
        FIXTURE,
        devel,
        planned_output_dependencies(planned, devel_plan),
        BTreeSet::from([devel_plan.package_name.clone(), "pkgconfig(cast-split)".to_owned()]),
    );
    for (plan, image) in [(docs_plan, docs), (debug_plan, debug)] {
        assert_exact_relations(
            FIXTURE,
            image,
            planned_output_dependencies(planned, plan),
            BTreeSet::from([plan.package_name.clone()]),
        );
    }
    assert_debug_output(FIXTURE, debug, &[executable_elf, library_elf]);

    let soname = "soname(libcast-split.so.1(x86_64))";
    assert!(
        root.meta
            .dependencies
            .iter()
            .map(Dependency::to_name)
            .any(|value| value == soname),
        "split: executable output does not depend on its shared-library SONAME"
    );
    assert!(
        libs.meta
            .providers
            .iter()
            .map(Provider::to_name)
            .any(|value| value == soname),
        "split: library output does not provide its SONAME"
    );
    assert!(
        devel
            .meta
            .providers
            .iter()
            .map(Provider::to_name)
            .any(|value| value == "pkgconfig(cast-split)"),
        "split: development output does not provide its pkg-config module"
    );
    assert!(
        devel
            .meta
            .dependencies
            .iter()
            .map(Dependency::to_name)
            .any(|value| value == libs_plan.package_name),
        "split: development package metadata omits the declared library output relation"
    );
    assert!(matches!(
        devel_plan.runtime_inputs.as_slice(),
        [OutputRelation::Planned { output }] if output == "libs"
    ));
}

fn assert_debug_output(fixture: &str, image: &PackageImage, originals: &[NativeElf]) {
    let expected_targets = originals
        .iter()
        .map(|original| debug_target(&original.build_id))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        image.layouts.keys().cloned().collect::<BTreeSet<_>>(),
        expected_targets,
        "{fixture}: debug output is not the exact build-ID closure of its native ELFs"
    );
    assert_eq!(
        expected_targets.len(),
        originals.len(),
        "{fixture}: distinct native ELFs unexpectedly share a build ID"
    );

    for original in originals {
        let target = debug_target(&original.build_id);
        let layout = &image.layouts[&target];
        assert_eq!(layout.mode & 0o777, 0o644);
        let debug_bytes = regular_bytes(fixture, image, &target);
        assert_debug_elf(fixture, &target, debug_bytes, original);
        let filename = Path::new(&target)
            .file_name()
            .and_then(|name| name.to_str())
            .expect("validated UTF-8 build-ID target");
        assert_eq!(
            original.debug_link.basename, filename,
            "{fixture}: runtime ELF .gnu_debuglink does not name its build-ID debug file"
        );
        assert_eq!(
            original.debug_link.crc32,
            gnu_debuglink_crc32(debug_bytes),
            "{fixture}: runtime ELF .gnu_debuglink CRC does not authenticate its debug file"
        );
    }
    assert_no_directories(fixture, "dbginfo", image);
}

fn assert_leaf_paths<'a>(
    fixture: &str,
    output: &str,
    image: &PackageImage,
    expected: impl IntoIterator<Item = &'a str>,
) {
    assert_eq!(
        image
            .layouts
            .iter()
            .filter(|(_, layout)| !matches!(&layout.file, StonePayloadLayoutFile::Directory(_)))
            .map(|(target, _)| target.as_str())
            .collect::<BTreeSet<_>>(),
        expected.into_iter().collect(),
        "{fixture}: {output} output leaf classification drift"
    );
}

fn assert_no_directories(fixture: &str, output: &str, image: &PackageImage) {
    assert!(
        image
            .layouts
            .values()
            .all(|layout| !matches!(&layout.file, StonePayloadLayoutFile::Directory(_))),
        "{fixture}: {output} unexpectedly emitted a non-empty normal ancestor directory"
    );
}

fn regular_bytes<'a>(fixture: &str, image: &'a PackageImage, target: &str) -> &'a [u8] {
    let layout = image
        .layouts
        .get(target)
        .unwrap_or_else(|| panic!("{fixture}: missing regular layout /usr/{target}"));
    let StonePayloadLayoutFile::Regular(digest, _) = &layout.file else {
        panic!("{fixture}: /usr/{target} is not a regular file")
    };
    image.content[digest].as_slice()
}

fn assert_regular(fixture: &str, image: &PackageImage, target: &str, permissions: u32, expected: Vec<u8>) {
    assert_eq!(
        image.layouts[target].mode & 0o777,
        permissions,
        "{fixture}: /usr/{target} permissions drift"
    );
    assert_eq!(regular_bytes(fixture, image, target), expected);
}

fn assert_symlink(fixture: &str, image: &PackageImage, target: &str, expected_source: &str) {
    let StonePayloadLayoutFile::Symlink(source, _) = &image.layouts[target].file else {
        panic!("{fixture}: /usr/{target} is not a symlink")
    };
    assert_eq!(
        source.as_str(),
        expected_source,
        "{fixture}: /usr/{target} symlink source drift"
    );
    assert_eq!(image.layouts[target].mode & 0o777, 0o777);
}

#[derive(Debug, Clone, Copy)]
enum RuntimeElfKind {
    Executable,
    SharedLibrary,
}

#[derive(Debug)]
struct DebugLink {
    basename: String,
    crc32: u32,
}

#[derive(Debug)]
struct NativeElf {
    elf_type: u16,
    build_id: String,
    dependencies: BTreeSet<String>,
    soname: Option<String>,
    debug_link: DebugLink,
}

fn parse_structural_elf<'a>(fixture: &str, target: &str, bytes: &'a [u8]) -> ElfBytes<'a, AnyEndian> {
    let elf = ElfBytes::<AnyEndian>::minimal_parse(bytes)
        .unwrap_or_else(|error| panic!("{fixture}: structurally parse /usr/{target} as ELF: {error}"));
    assert_eq!(elf.ehdr.class, Class::ELF64, "{fixture}: /usr/{target} is not ELF64");
    assert!(
        elf.ehdr.endianness.is_little(),
        "{fixture}: /usr/{target} is not a little-endian ELF"
    );
    assert_eq!(
        elf.ehdr.version,
        u32::from(EV_CURRENT),
        "{fixture}: /usr/{target} has an unsupported ELF version"
    );
    assert_eq!(
        elf.ehdr.e_machine, EM_X86_64,
        "{fixture}: /usr/{target} is not an x86_64 ELF"
    );
    let sections = elf
        .section_headers()
        .unwrap_or_else(|| panic!("{fixture}: /usr/{target} has no section-header table"));
    assert!(
        sections.iter().count() > 1,
        "{fixture}: /usr/{target} has no meaningful ELF sections"
    );
    // Resolve the section-name table too; minimal_parse intentionally leaves
    // both tables lazy, so this turns malformed table geometry into a failure.
    elf.section_headers_with_strtab()
        .unwrap_or_else(|error| panic!("{fixture}: parse /usr/{target} section table and names: {error}"));
    elf
}

fn assert_runtime_elf(fixture: &str, target: &str, bytes: &[u8], kind: RuntimeElfKind) -> NativeElf {
    let elf = parse_structural_elf(fixture, target, bytes);
    match kind {
        RuntimeElfKind::Executable => {
            assert!(
                matches!(elf.ehdr.e_type, ET_EXEC | ET_DYN),
                "{fixture}: /usr/{target} is neither ET_EXEC nor a PIE ET_DYN"
            );
            assert_ne!(
                elf.ehdr.e_entry, 0,
                "{fixture}: /usr/{target} has no executable entry point"
            );
        }
        RuntimeElfKind::SharedLibrary => assert_eq!(
            elf.ehdr.e_type, ET_DYN,
            "{fixture}: /usr/{target} shared library is not ET_DYN"
        ),
    }

    let segments = elf
        .segments()
        .unwrap_or_else(|| panic!("{fixture}: runtime ELF /usr/{target} has no program-header table"))
        .iter()
        .collect::<Vec<_>>();
    assert!(
        segments.iter().any(|segment| segment.p_type == PT_LOAD),
        "{fixture}: runtime ELF /usr/{target} has no loadable segment"
    );
    for segment in &segments {
        assert!(
            segment.p_memsz >= segment.p_filesz,
            "{fixture}: runtime ELF /usr/{target} has a segment larger on disk than in memory"
        );
        assert!(
            segment.p_align <= 1 || segment.p_align.is_power_of_two(),
            "{fixture}: runtime ELF /usr/{target} has an invalid segment alignment"
        );
        elf.segment_data(segment)
            .unwrap_or_else(|error| panic!("{fixture}: runtime ELF /usr/{target} segment escapes file: {error}"));
    }

    let interp = elf
        .section_header_by_name(".interp")
        .unwrap_or_else(|error| panic!("{fixture}: inspect /usr/{target} interpreter section: {error}"));
    match kind {
        RuntimeElfKind::Executable => {
            assert!(
                segments.iter().any(|segment| segment.p_type == PT_INTERP),
                "{fixture}: executable /usr/{target} has no PT_INTERP program header"
            );
            assert!(
                interp.is_some(),
                "{fixture}: executable /usr/{target} has no .interp section"
            );
        }
        RuntimeElfKind::SharedLibrary => {
            assert!(
                interp.is_none(),
                "{fixture}: shared library /usr/{target} unexpectedly has .interp"
            )
        }
    }

    let build_id = elf_build_id(fixture, target, &elf);
    assert_eq!(
        build_id.len(),
        40,
        "{fixture}: /usr/{target} does not use a SHA-1 GNU build ID"
    );
    assert!(
        build_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{fixture}: /usr/{target} GNU build ID is not lowercase hexadecimal"
    );

    let (dependencies, soname) = runtime_elf_relations(fixture, target, &elf, interp.as_ref());
    if matches!(kind, RuntimeElfKind::SharedLibrary) {
        assert!(
            soname.is_some(),
            "{fixture}: shared library /usr/{target} has no DT_SONAME"
        );
    } else {
        assert!(
            soname.is_none(),
            "{fixture}: executable /usr/{target} unexpectedly has DT_SONAME"
        );
    }

    NativeElf {
        elf_type: elf.ehdr.e_type,
        build_id,
        dependencies,
        soname,
        debug_link: parse_debug_link(fixture, target, &elf),
    }
}

fn runtime_elf_relations(
    fixture: &str,
    target: &str,
    elf: &ElfBytes<'_, AnyEndian>,
    interp: Option<&elf::section::SectionHeader>,
) -> (BTreeSet<String>, Option<String>) {
    let mut needed = Vec::new();
    let mut soname = None;
    let dynamic = elf
        .dynamic()
        .unwrap_or_else(|error| panic!("{fixture}: parse /usr/{target} dynamic table: {error}"))
        .unwrap_or_else(|| panic!("{fixture}: runtime ELF /usr/{target} has no dynamic table"));
    for entry in dynamic.iter() {
        match entry.d_tag {
            DT_NEEDED => needed.push(usize::try_from(entry.d_val()).unwrap()),
            DT_SONAME => soname = Some(usize::try_from(entry.d_val()).unwrap()),
            _ => {}
        }
    }
    let (_, strings) = elf
        .dynamic_symbol_table()
        .unwrap_or_else(|error| panic!("{fixture}: parse /usr/{target} dynamic strings: {error}"))
        .unwrap_or_else(|| panic!("{fixture}: runtime ELF /usr/{target} has no dynamic string table"));
    let mut dependencies = needed
        .into_iter()
        .map(|offset| {
            let name = strings
                .get(offset)
                .unwrap_or_else(|error| panic!("{fixture}: resolve /usr/{target} DT_NEEDED: {error}"));
            format!("soname({name}(x86_64))")
        })
        .collect::<BTreeSet<_>>();
    if let Some(section) = interp {
        let (data, compression) = elf
            .section_data(section)
            .unwrap_or_else(|error| panic!("{fixture}: read /usr/{target} .interp: {error}"));
        assert!(
            compression.is_none(),
            "{fixture}: /usr/{target} has a compressed .interp section"
        );
        let interpreter = CStr::from_bytes_until_nul(data)
            .unwrap_or_else(|error| panic!("{fixture}: /usr/{target} .interp is not NUL-terminated: {error}"))
            .to_str()
            .unwrap_or_else(|error| panic!("{fixture}: /usr/{target} .interp is not UTF-8: {error}"));
        assert_eq!(interpreter.as_bytes().len() + 1, data.len());
        dependencies.insert(format!("interpreter({interpreter}(x86_64))"));
    }
    let soname = soname.map(|offset| {
        strings
            .get(offset)
            .unwrap_or_else(|error| panic!("{fixture}: resolve /usr/{target} DT_SONAME: {error}"))
            .to_owned()
    });
    (dependencies, soname)
}

fn elf_build_id(fixture: &str, target: &str, elf: &ElfBytes<'_, AnyEndian>) -> String {
    let section = elf
        .section_header_by_name(".note.gnu.build-id")
        .unwrap_or_else(|error| panic!("{fixture}: inspect /usr/{target} GNU build-ID section: {error}"))
        .unwrap_or_else(|| panic!("{fixture}: /usr/{target} has no GNU build-ID section"));
    let notes = elf
        .section_data_as_notes(&section)
        .unwrap_or_else(|error| panic!("{fixture}: parse /usr/{target} GNU build-ID notes: {error}"));
    let build_ids = notes
        .filter_map(|note| match note {
            Note::GnuBuildId(build_id) => Some(hex::encode(build_id.0)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        build_ids.len(),
        1,
        "{fixture}: /usr/{target} must contain exactly one GNU build ID"
    );
    build_ids.into_iter().next().unwrap()
}

fn parse_debug_link(fixture: &str, target: &str, elf: &ElfBytes<'_, AnyEndian>) -> DebugLink {
    let section = elf
        .section_header_by_name(".gnu_debuglink")
        .unwrap_or_else(|error| panic!("{fixture}: inspect /usr/{target} .gnu_debuglink: {error}"))
        .unwrap_or_else(|| panic!("{fixture}: analyzed runtime ELF /usr/{target} has no .gnu_debuglink"));
    let (data, compression) = elf
        .section_data(&section)
        .unwrap_or_else(|error| panic!("{fixture}: read /usr/{target} .gnu_debuglink: {error}"));
    assert!(
        compression.is_none(),
        "{fixture}: /usr/{target} has a compressed .gnu_debuglink"
    );
    let nul = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or_else(|| panic!("{fixture}: /usr/{target} .gnu_debuglink has no NUL terminator"));
    let basename = std::str::from_utf8(&data[..nul])
        .unwrap_or_else(|error| panic!("{fixture}: /usr/{target} .gnu_debuglink basename is not UTF-8: {error}"));
    assert_eq!(
        Path::new(basename).file_name().and_then(|name| name.to_str()),
        Some(basename),
        "{fixture}: /usr/{target} .gnu_debuglink is not a basename"
    );
    let crc_offset = nul
        .checked_add(4)
        .map(|value| value & !3)
        .expect("debuglink padding offset overflow");
    assert_eq!(
        data.len(),
        crc_offset + 4,
        "{fixture}: /usr/{target} .gnu_debuglink has non-canonical padding/cardinality"
    );
    assert!(
        data[nul + 1..crc_offset].iter().all(|byte| *byte == 0),
        "{fixture}: /usr/{target} .gnu_debuglink padding is not zeroed"
    );
    let crc32 = u32::from_le_bytes(data[crc_offset..].try_into().unwrap());
    DebugLink {
        basename: basename.to_owned(),
        crc32,
    }
}

fn assert_debug_elf(fixture: &str, target: &str, bytes: &[u8], original: &NativeElf) {
    let elf = parse_structural_elf(fixture, target, bytes);
    assert_eq!(
        elf.ehdr.e_type, original.elf_type,
        "{fixture}: debug ELF /usr/{target} type differs from its runtime ELF"
    );
    assert_eq!(
        elf_build_id(fixture, target, &elf),
        original.build_id,
        "{fixture}: debug ELF /usr/{target} build ID differs from its runtime ELF"
    );
    for alternatives in [[".debug_info", ".zdebug_info"], [".debug_line", ".zdebug_line"]] {
        assert!(
            alternatives.iter().any(|name| {
                elf.section_header_by_name(name)
                    .unwrap_or_else(|error| panic!("{fixture}: inspect /usr/{target} debug section {name}: {error}"))
                    .is_some()
            }),
            "{fixture}: debug ELF /usr/{target} lacks {} data",
            alternatives[0]
        );
    }
}

fn debug_target(build_id: &str) -> String {
    assert!(
        build_id.len() >= 2,
        "GNU build ID is too short for the build-ID hierarchy"
    );
    format!("lib/debug/.build-id/{}/{}.debug", &build_id[..2], &build_id[2..])
}

fn gnu_debuglink_crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let polynomial = 0xedb8_8320 & 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ polynomial;
        }
    }
    !crc
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
}

fn fixture_source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gluon/execution/source-trees")
}

fn tracked_bytes(tree: &str, relative: &str) -> Vec<u8> {
    let root_path = fixture_source_root();
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&root_path)
        .unwrap_or_else(|error| panic!("open tracked fixture root {root_path:?} without following links: {error}"));
    assert!(
        root.metadata()
            .unwrap_or_else(|error| panic!("inspect tracked fixture root {root_path:?}: {error}"))
            .file_type()
            .is_dir(),
        "tracked fixture root {root_path:?} is not a directory"
    );

    let relative_path = Path::new(tree).join(relative);
    let components = relative_path
        .components()
        .map(|component| match component {
            Component::Normal(component) => component,
            _ => panic!("tracked source fixture path {relative_path:?} is not normalized and relative"),
        })
        .collect::<Vec<_>>();
    assert!(!components.is_empty(), "tracked source fixture path is empty");

    let mut parent = root;
    for component in &components[..components.len() - 1] {
        let component = CString::new(component.as_bytes())
            .unwrap_or_else(|_| panic!("tracked fixture directory component contains NUL"));
        // SAFETY: the parent is a live authenticated directory descriptor and
        // the component is normalized. O_NOFOLLOW rejects substituted links.
        let descriptor = unsafe {
            nix::libc::openat(
                parent.as_raw_fd(),
                component.as_ptr(),
                nix::libc::O_RDONLY
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK,
            )
        };
        assert!(
            descriptor >= 0,
            "open tracked fixture directory {relative_path:?} without following links: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: openat returned a fresh owned descriptor.
        parent = unsafe { File::from_raw_fd(descriptor) };
        let metadata = parent
            .metadata()
            .unwrap_or_else(|error| panic!("inspect tracked fixture directory {relative_path:?}: {error}"));
        assert!(
            metadata.file_type().is_dir(),
            "tracked fixture ancestor is not a directory"
        );
    }

    let component = CString::new(components.last().unwrap().as_bytes())
        .unwrap_or_else(|_| panic!("tracked fixture file component contains NUL"));
    // SAFETY: the parent descriptor is live and the final component is
    // normalized. O_NOFOLLOW prevents a final-component symlink substitution.
    let descriptor = unsafe {
        nix::libc::openat(
            parent.as_raw_fd(),
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
        "open tracked source fixture {relative_path:?} without following links: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: openat returned a fresh owned descriptor.
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    let before = file
        .metadata()
        .unwrap_or_else(|error| panic!("inspect opened tracked source fixture {relative_path:?}: {error}"));
    assert!(
        before.file_type().is_file(),
        "tracked source fixture {relative_path:?} is not a regular file"
    );
    assert_eq!(
        before.nlink(),
        1,
        "tracked source fixture {relative_path:?} must have exactly one hard link"
    );
    assert_eq!(
        before.mode() & nix::libc::S_IFMT,
        nix::libc::S_IFREG,
        "tracked source fixture {relative_path:?} mode/type mismatch"
    );
    assert_eq!(
        before.mode() & 0o7000,
        0,
        "tracked source fixture {relative_path:?} must not carry special mode bits"
    );
    assert_eq!(
        before.mode() & 0o113,
        0,
        "tracked source fixture {relative_path:?} must be non-executable and non-world-writable"
    );
    assert!(
        before.size() <= MAX_TRACKED_FIXTURE_BYTES,
        "tracked source fixture {relative_path:?} exceeds its boundary"
    );
    let stamp = FileStamp::from_metadata(&before);
    let bytes = read_bounded(
        "tracked",
        &format!("source fixture {relative_path:?}"),
        &mut file,
        MAX_TRACKED_FIXTURE_BYTES,
    );
    assert_eq!(u64::try_from(bytes.len()).unwrap(), before.size());
    let after = file
        .metadata()
        .unwrap_or_else(|error| panic!("reinspect tracked source fixture {relative_path:?}: {error}"));
    assert_eq!(
        FileStamp::from_metadata(&after),
        stamp,
        "tracked source fixture {relative_path:?} changed while reading"
    );
    bytes
}

fn expected_split_pkgconfig() -> Vec<u8> {
    let template = String::from_utf8(tracked_bytes("cast-split-fixture-1.0.0", "cast-split.pc.in"))
        .expect("tracked pkg-config template must remain UTF-8");
    template
        .replace("@CMAKE_INSTALL_PREFIX@", "/usr")
        .replace("@CMAKE_INSTALL_LIBDIR@", "lib")
        .replace("@CMAKE_INSTALL_INCLUDEDIR@", "include")
        .replace("@PROJECT_VERSION@", "1.0.0")
        .into_bytes()
}
