const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const EMPTY_FILE_DIGEST: u128 = 0x99aa_06d3_0147_98d8_6001_c324_468d_497f;
const MAX_FROZEN_EXECUTABLE_PACKAGES: usize = 4_096;
const MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES: usize = MIB as usize;
const MAX_FROZEN_EXECUTABLE_BINDINGS: usize = 4_096;
// Linux PATH_MAX includes the terminating NUL; frozen paths and link targets
// are stored without it.
const MAX_FROZEN_EXECUTABLE_PATH_BYTES: usize = nix::libc::PATH_MAX as usize - 1;
// Tree::structured_children recursively descends path components. Keep frozen
// layouts well below the stack depth that PATH_MAX alone could permit.
const MAX_FROZEN_LAYOUT_PATH_COMPONENTS: usize = 128;
const MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES: usize = 16 * MIB as usize;
const MAX_FROZEN_EXECUTABLE_LAYOUTS: usize = 262_144;
const MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES: usize = 64 * MIB as usize;
const MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS: usize = 262_144;
const MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES: usize = 64 * MIB as usize;
const MAX_FROZEN_EXECUTABLE_BYTES: u64 = 512 * MIB;
const MAX_TOTAL_FROZEN_EXECUTABLE_BYTES: u64 = 2 * GIB;
const MAX_FROZEN_EXECUTABLE_SYMLINKS: usize = 32;
const MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES: usize = nix::libc::PATH_MAX as usize - 1;
// Linux inspects at most 256 bytes of a script header. Requiring the newline
// inside that same finite window avoids kernel-version-dependent truncation
// and leaves at most 253 bytes for `#!` plus one absolute interpreter path.
const MAX_FROZEN_SHEBANG_LINE_BYTES: usize = 256;
const MAX_FROZEN_SHEBANG_INTERPRETER_BYTES: usize = MAX_FROZEN_SHEBANG_LINE_BYTES - 3;
// Linux's binary-parameter recursion ceiling admits five nested scripts and
// rejects the sixth with ELOOP. Keep the script-specific counter identical to
// the kernel; ELF PT_INTERP edges have their own finite graph ceiling below.
const MAX_FROZEN_SHEBANG_INTERPRETERS: usize = 5;
const MAX_FROZEN_EXECUTABLE_INTERPRETERS: usize = 32;
const MAX_FROZEN_ELF_PROGRAM_HEADERS: usize = 1_024;
const MAX_FROZEN_ELF_INTERPRETER_BYTES: usize = MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1;
// Descriptors are retained until the complete graph is revalidated. Keep a
// conservative ceiling below the common 1024-descriptor process limit so the
// verifier fails deliberately rather than through ambient EMFILE pressure.
const MAX_FROZEN_EXECUTABLE_PINNED_FILES: usize = 512;
const FROZEN_EXECUTABLE_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(120);
const FROZEN_MATERIALIZATION_TIMEOUT: Duration = Duration::from_secs(600);
const FROZEN_NAMESPACE_RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const FROZEN_DESTINATION_LOCK_RETRY: Duration = Duration::from_millis(10);
const MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS: usize = 1_024;
const MAX_FROZEN_PRIVATE_DIRECTORY_ATTEMPTS: usize = 128;
const FROZEN_PRIVATE_DIRECTORY_RANDOM_BYTES: usize = 16;
// Independent frozen-root copies densify every regular output inode. Match the
// existing Mason archive-staging ceiling: one cached asset remains bounded at
// 8 GiB and the complete copied userspace at 32 GiB of logical file bytes.
const MAX_TOTAL_FROZEN_BLIT_BYTES: u64 = 32 * GIB;
const MAX_FROZEN_NORMALIZED_INODES: usize = MAX_FROZEN_EXECUTABLE_LAYOUTS + MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS + 6;

#[derive(Debug, Default)]
struct FrozenCopyManifest {
    lengths: BTreeMap<u128, u64>,
    total_bytes: u64,
}

impl FrozenCopyManifest {
    fn from_tree(installation: &Installation, tree: &vfs::Tree<PendingFile>, deadline: Instant) -> Result<Self, Error> {
        Self::from_tree_with_limit(installation, tree, deadline, MAX_TOTAL_FROZEN_BLIT_BYTES)
    }

    fn from_tree_with_limit(
        installation: &Installation,
        tree: &vfs::Tree<PendingFile>,
        deadline: Instant,
        limit: u64,
    ) -> Result<Self, Error> {
        let mut digests = tree.iter().filter_map(|item| match item.layout.file {
            StonePayloadLayoutFile::Regular(digest, _) if digest != EMPTY_FILE_DIGEST => Some(digest),
            _ => None,
        });
        let Some(first) = digests.next() else {
            return Ok(Self::default());
        };
        let pool = AssetPool::open(installation)?;
        let manifest = Self::from_digests_with_limit(std::iter::once(first).chain(digests), limit, |digest| {
            require_frozen_materialization_deadline(deadline)?;
            let asset = pool.open_asset(&frozen_asset_path(digest))?;
            Ok(asset.witness.length)
        })?;
        require_frozen_materialization_deadline(deadline)?;
        pool.revalidate()?;
        Ok(manifest)
    }

    fn from_digests_with_limit(
        digests: impl IntoIterator<Item = u128>,
        limit: u64,
        mut length: impl FnMut(u128) -> Result<u64, Error>,
    ) -> Result<Self, Error> {
        let mut manifest = Self::default();
        for digest in digests {
            if digest == EMPTY_FILE_DIGEST {
                continue;
            }
            let actual = length(digest)?;
            if let Some(expected) = manifest.lengths.get(&digest) {
                if *expected != actual {
                    return Err(Error::FrozenMaterializationAssetLengthChanged {
                        digest,
                        expected: *expected,
                        actual,
                    });
                }
            } else {
                manifest.lengths.insert(digest, actual);
            }
            account_frozen_blit_bytes(&mut manifest.total_bytes, actual, limit)?;
        }
        Ok(manifest)
    }

    fn require_length(&self, digest: u128, actual: u64) -> Result<(), Error> {
        match self.lengths.get(&digest) {
            Some(expected) if *expected == actual => Ok(()),
            Some(expected) => Err(Error::FrozenMaterializationAssetLengthChanged {
                digest,
                expected: *expected,
                actual,
            }),
            None => Err(Error::FrozenMaterializationAssetMissingFromManifest { digest }),
        }
    }
}

fn account_frozen_blit_bytes(total: &mut u64, additional: u64, limit: u64) -> Result<(), Error> {
    let actual = total
        .checked_add(additional)
        .ok_or(Error::FrozenMaterializationTotalByteLimit {
            limit,
            actual: u64::MAX,
        })?;
    if actual > limit {
        return Err(Error::FrozenMaterializationTotalByteLimit { limit, actual });
    }
    *total = actual;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct FrozenNormalizationLimits {
    inodes: usize,
    depth: usize,
}

impl FrozenNormalizationLimits {
    const PRODUCTION: Self = Self {
        inodes: MAX_FROZEN_NORMALIZED_INODES,
        depth: MAX_FROZEN_LAYOUT_PATH_COMPONENTS,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FrozenExpectedKind {
    Directory,
    Regular { digest: u128 },
    Symlink { target: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenExpectedEntry {
    kind: FrozenExpectedKind,
    mode: u32,
}

#[derive(Debug)]
struct FrozenExpectedTree {
    entries: BTreeMap<PathBuf, FrozenExpectedEntry>,
    children: BTreeMap<PathBuf, BTreeMap<OsString, PathBuf>>,
}

impl FrozenExpectedTree {
    fn from_vfs(tree: &vfs::Tree<PendingFile>, deadline: Instant) -> Result<Self, Error> {
        let mut entries = BTreeMap::new();
        Self::insert_entry(
            &mut entries,
            PathBuf::from("/"),
            FrozenExpectedEntry {
                kind: FrozenExpectedKind::Directory,
                mode: 0o755,
            },
        )?;

        for item in tree.iter() {
            require_frozen_materialization_deadline(deadline)?;
            let path = PathBuf::from(item.path().as_str());
            let kind = match &item.layout.file {
                StonePayloadLayoutFile::Directory(_) => FrozenExpectedKind::Directory,
                StonePayloadLayoutFile::Regular(digest, _) => FrozenExpectedKind::Regular { digest: *digest },
                StonePayloadLayoutFile::Symlink(target, _) => FrozenExpectedKind::Symlink {
                    target: target.as_bytes().to_vec(),
                },
                StonePayloadLayoutFile::CharacterDevice(_)
                | StonePayloadLayoutFile::BlockDevice(_)
                | StonePayloadLayoutFile::Fifo(_)
                | StonePayloadLayoutFile::Socket(_)
                | StonePayloadLayoutFile::Unknown(..) => {
                    return Err(Error::InvalidFrozenNormalizationDeclaration {
                        path,
                        reason: "the declarative tree contains an unsupported inode type",
                    });
                }
            };
            Self::insert_entry(
                &mut entries,
                path,
                FrozenExpectedEntry {
                    kind,
                    mode: item.layout.mode & 0o7777,
                },
            )?;
        }

        for (target, name) in ROOT_ABI_LINKS {
            require_frozen_materialization_deadline(deadline)?;
            Self::insert_entry(
                &mut entries,
                Path::new("/").join(name),
                FrozenExpectedEntry {
                    kind: FrozenExpectedKind::Symlink {
                        target: target.as_bytes().to_vec(),
                    },
                    mode: 0o777,
                },
            )?;
        }
        require_frozen_materialization_deadline(deadline)?;
        Self::from_entries(entries, FrozenNormalizationLimits::PRODUCTION)
    }

    fn insert_entry(
        entries: &mut BTreeMap<PathBuf, FrozenExpectedEntry>,
        path: PathBuf,
        entry: FrozenExpectedEntry,
    ) -> Result<(), Error> {
        match entries.entry(path.clone()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(entry);
                Ok(())
            }
            std::collections::btree_map::Entry::Occupied(slot) if slot.get() == &entry => Ok(()),
            std::collections::btree_map::Entry::Occupied(_) => Err(Error::InvalidFrozenNormalizationDeclaration {
                path,
                reason: "two declarations disagree about one path",
            }),
        }
    }

    fn from_entries(
        entries: BTreeMap<PathBuf, FrozenExpectedEntry>,
        limits: FrozenNormalizationLimits,
    ) -> Result<Self, Error> {
        let actual = entries.len();
        if actual > limits.inodes {
            return Err(Error::FrozenNormalizationInodeLimit {
                limit: limits.inodes,
                actual,
            });
        }
        let Some(root) = entries.get(Path::new("/")) else {
            return Err(Error::InvalidFrozenNormalizationDeclaration {
                path: PathBuf::from("/"),
                reason: "the declarative tree has no root",
            });
        };
        if root
            != &(FrozenExpectedEntry {
                kind: FrozenExpectedKind::Directory,
                mode: 0o755,
            })
        {
            return Err(Error::InvalidFrozenNormalizationDeclaration {
                path: PathBuf::from("/"),
                reason: "the declarative root is not a mode-0755 directory",
            });
        }

        let mut children: BTreeMap<PathBuf, BTreeMap<OsString, PathBuf>> = BTreeMap::new();
        for path in entries.keys() {
            let depth =
                frozen_normalization_path_depth(path).ok_or_else(|| Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative path is not normalized and absolute",
                })?;
            if depth > limits.depth {
                return Err(Error::FrozenNormalizationDepthLimit {
                    limit: limits.depth,
                    actual: depth,
                });
            }
            if path == Path::new("/") {
                continue;
            }
            let parent = path
                .parent()
                .ok_or_else(|| Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative path has no parent",
                })?;
            let Some(parent_entry) = entries.get(parent) else {
                return Err(Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative path has no declared parent",
                });
            };
            if !matches!(parent_entry.kind, FrozenExpectedKind::Directory) {
                return Err(Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative path has a non-directory parent",
                });
            }
            let name = path
                .file_name()
                .ok_or_else(|| Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative path has no final component",
                })?
                .to_owned();
            if children
                .entry(parent.to_owned())
                .or_default()
                .insert(name, path.clone())
                .is_some()
            {
                return Err(Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative directory contains a duplicate name",
                });
            }
        }
        Ok(Self { entries, children })
    }

    fn entry(&self, path: &Path) -> Result<&FrozenExpectedEntry, Error> {
        self.entries
            .get(path)
            .ok_or_else(|| Error::InvalidFrozenNormalizationDeclaration {
                path: path.to_owned(),
                reason: "the normalizer requested an undeclared path",
            })
    }

    fn children(&self, path: &Path) -> impl Iterator<Item = (&OsString, &PathBuf)> {
        self.children.get(path).into_iter().flat_map(BTreeMap::iter)
    }
}

fn frozen_normalization_path_depth(path: &Path) -> Option<usize> {
    if !path.is_absolute() {
        return None;
    }
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            PathComponent::RootDir => {}
            PathComponent::Normal(_) => depth = depth.saturating_add(1),
            PathComponent::CurDir | PathComponent::ParentDir | PathComponent::Prefix(_) => return None,
        }
    }
    Some(depth)
}

fn frozen_normalization_declared_children<'a>(
    expected: &'a FrozenExpectedTree,
    path: &Path,
) -> Result<Vec<(&'a OsString, &'a PathBuf)>, Error> {
    let count = expected.children.get(path).map_or(0, BTreeMap::len);
    let mut children = Vec::new();
    children
        .try_reserve_exact(count)
        .map_err(|source| Error::ReserveFrozenNormalizationInventory {
            path: path.to_owned(),
            source,
        })?;
    children.extend(expected.children(path));
    Ok(children)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenNormalizationCheckpoint {
    EntryPinned,
    DirectoryTraversalModeApplied,
    DirectoryEnumerated,
    BeforeFinalTreeConfirmation,
    AfterRegularDigest,
    BeforeDirectoryFinalInventory,
    AfterDirectoryFinalInventory,
    BeforeEntryRevalidation,
    BeforeRootRevalidation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenNormalizationOpen {
    Anchor,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenNormalizationWitness {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    length: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenNormalizationFinalWitness {
    stable: FrozenNormalizationWitness,
    accessed_seconds: i64,
    accessed_nanoseconds: i64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FrozenNormalizationFinalWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            stable: FrozenNormalizationWitness::from_metadata(metadata),
            accessed_seconds: metadata.atime(),
            accessed_nanoseconds: metadata.atime_nsec(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

impl FrozenNormalizationWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
            links: metadata.nlink(),
            length: metadata.len(),
        }
    }

    fn with_permissions(self, mode: u32) -> Self {
        Self {
            mode: (self.mode & nix::libc::S_IFMT) | mode,
            ..self
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenNormalizationInventoryEntry {
    name: CString,
    witness: FrozenNormalizationWitness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenExecutableCheckpoint {
    AfterOpen,
    AfterDigest,
    BeforeReopen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenExecutableWitness {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

/// Stable root-inode properties which remain invariant while callers add
/// build-visible descendants.  Directory mtime/ctime/link-count deliberately
/// do not participate: creating source and mount-target directories changes
/// those values without changing the root inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenRootIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
}

impl FrozenRootIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
        }
    }
}

impl FrozenExecutableWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone)]
struct ExpectedFrozenExecutable {
    digest: u128,
    mode: u32,
    resolved_path: PathBuf,
    symlinks: Vec<ExpectedFrozenSymlink>,
}

#[derive(Debug, Clone)]
struct ExpectedFrozenSymlink {
    package: package::Id,
    path: PathBuf,
    target: String,
    mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FrozenExecutableLayout {
    Regular { digest: u128, mode: u32 },
    Symlink { target: String, mode: u32 },
    Directory { uid: u32, gid: u32, mode: u32, tag: u32 },
    Other,
}

impl FrozenExecutableLayout {
    fn is_identical_directory(&self, other: &Self) -> bool {
        matches!((self, other), (Self::Directory { .. }, Self::Directory { .. })) && self == other
    }
}

#[derive(Debug)]
struct PinnedFrozenSymlink {
    file: fs::File,
    witness: FrozenExecutableWitness,
    expected: ExpectedFrozenSymlink,
}

#[derive(Debug)]
struct PinnedFrozenExecutable {
    file: fs::File,
    witness: FrozenExecutableWitness,
    binding: FrozenExecutableBinding,
    expected: ExpectedFrozenExecutable,
    symlinks: Vec<PinnedFrozenSymlink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenShebangInterpreter {
    path: PathBuf,
    root_alias: Option<ExpectedFrozenRootAlias>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpectedFrozenRootAlias {
    path: PathBuf,
    target: String,
}

#[derive(Debug)]
struct PinnedFrozenRootAlias {
    file: fs::File,
    witness: FrozenExecutableWitness,
    expected: ExpectedFrozenRootAlias,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenShebangParseError {
    LineTooLong,
    Unterminated,
    EmptyInterpreter,
    InterpreterTooLong,
    Nul,
    WhitespaceOrOptions,
    NonUtf8,
    Relative,
    NonNormalized,
    EnvironmentLookup,
}

impl FrozenShebangParseError {
    fn reason(self) -> &'static str {
        match self {
            Self::LineTooLong => "the shebang line exceeds the 256-byte kernel inspection window",
            Self::Unterminated => "the shebang line is not newline-terminated",
            Self::EmptyInterpreter => "the shebang does not name an interpreter",
            Self::InterpreterTooLong => "the interpreter path exceeds the shebang path limit",
            Self::Nul => "the interpreter path contains NUL",
            Self::WhitespaceOrOptions => "whitespace and interpreter options are not supported",
            Self::NonUtf8 => "the interpreter path is not UTF-8",
            Self::Relative => "the interpreter path is not absolute",
            Self::NonNormalized => "the interpreter path is not lexically normalized",
            Self::EnvironmentLookup => "environment-based interpreter lookup is forbidden",
        }
    }
}

#[derive(Debug)]
struct FrozenExecutableDigest {
    digest: u128,
    shebang_probe: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FrozenExecutableInterpreter {
    Shebang(FrozenShebangInterpreter),
    Elf(FrozenShebangInterpreter),
}

impl FrozenExecutableInterpreter {
    fn binding(&self) -> &FrozenShebangInterpreter {
        match self {
            Self::Shebang(binding) | Self::Elf(binding) => binding,
        }
    }

    fn is_shebang(&self) -> bool {
        matches!(self, Self::Shebang(_))
    }
}

#[derive(Debug)]
struct PreparedFrozenExecutableLayout {
    package: package::Id,
    path: PathBuf,
    entry: FrozenExecutableLayout,
    is_directory: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenLayoutQueryOperation {
    Materialization,
    ExecutableVerification,
}

impl FrozenLayoutQueryOperation {
    fn timeout(self) -> Error {
        match self {
            Self::Materialization => Error::FrozenMaterializationTimeout {
                seconds: FROZEN_MATERIALIZATION_TIMEOUT.as_secs(),
            },
            Self::ExecutableVerification => Error::FrozenExecutableVerificationTimeout {
                seconds: FROZEN_EXECUTABLE_VERIFICATION_TIMEOUT.as_secs(),
            },
        }
    }
}
