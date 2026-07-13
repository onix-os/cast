// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use std::{
    ffi::OsStr,
    fs::Metadata,
    io,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
};

use astr::AStr;
use fs_err as fs;
use glob::Pattern;
use nix::libc::{S_IFDIR, S_IRGRP, S_IROTH, S_IRWXU, S_IXGRP, S_IXOTH};
use snafu::{ResultExt as _, Snafu};
use stone::{StoneDigestWriter, StoneDigestWriterHasher, StonePayloadLayoutFile, StonePayloadLayoutRecord};
use stone_recipe::derivation::PathRuleKind;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Rule {
    pub pattern: String,
    pub package: String,
    pub kind: PathRuleKind,
}

impl Rule {
    pub fn matches(&self, path: &str, metadata: &Metadata) -> bool {
        let pattern_matches = self.pattern == path
            || Pattern::new(&self.pattern)
                .expect("collection glob was validated before frozen execution")
                .matches(path)
            // If the supplied pattern is for a directory we want to match anything that's inside said directory,
            // Do this by creating a recursive glob pattern by appending `**` if the pattern already ends in a `/` or `/**` if not
            || Pattern::new(format!("{}/**", self.pattern.strip_suffix("/").unwrap_or(&self.pattern)).as_str())
                .expect("derived collection glob must remain valid")
                .matches(path);

        pattern_matches
            && match self.kind {
                PathRuleKind::Any => true,
                PathRuleKind::Executable => metadata.is_file() && metadata.mode() & 0o111 != 0,
                PathRuleKind::Symlink => metadata.file_type().is_symlink(),
                PathRuleKind::Special => {
                    let file_type = metadata.file_type();
                    file_type.is_char_device()
                        || file_type.is_block_device()
                        || file_type.is_fifo()
                        || file_type.is_socket()
                }
            }
    }
}

#[derive(Debug)]
pub struct Collector {
    /// Rules stored in order of
    /// ascending priority
    rules: Vec<Rule>,
    root: PathBuf,
}

impl Collector {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            rules: vec![],
            root: root.into(),
        }
    }

    pub fn add_rule(&mut self, rule: Rule) {
        self.rules.push(rule);
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    fn matching_package(&self, path: &str, metadata: &Metadata) -> Option<&str> {
        // Rev = check highest priority rules first
        self.rules
            .iter()
            .rev()
            .find_map(|rule| rule.matches(path, metadata).then_some(rule.package.as_str()))
    }

    /// Produce a [`PathInfo`] from the provided [`Path`]
    pub fn path(&self, path: &Path, hasher: &mut StoneDigestWriterHasher) -> Result<PathInfo, Error> {
        let metadata = fs::symlink_metadata(path).context(IoSnafu)?;
        self.path_with_metadata(path.to_path_buf(), &metadata, hasher)
    }

    fn path_with_metadata(
        &self,
        path: PathBuf,
        metadata: &Metadata,
        hasher: &mut StoneDigestWriterHasher,
    ) -> Result<PathInfo, Error> {
        let target_path = Path::new("/").join(path.strip_prefix(&self.root).expect("path is ancestor of root"));

        let package = self
            .matching_package(target_path.to_str().unwrap_or_default(), metadata)
            .ok_or(Error::NoMatchingRule)?;

        PathInfo::new(path, target_path, metadata, hasher, package.to_owned())
    }

    /// Enumerates all paths from the filesystem starting at root or subdir of root, if provided
    pub fn enumerate_paths(
        &self,
        subdir: Option<(PathBuf, Metadata)>,
        hasher: &mut StoneDigestWriterHasher,
    ) -> Result<Vec<PathInfo>, Error> {
        let mut paths = vec![];

        let dir = subdir.as_ref().map(|t| t.0.as_path()).unwrap_or(&self.root);
        let mut entries: Vec<_> = fs::read_dir(dir)
            .context(IoSnafu)?
            .collect::<Result<Vec<_>, _>>()
            .context(IoSnafu)?;

        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let host_path = entry.path();
            let metadata = fs::symlink_metadata(&host_path).context(IoSnafu)?;

            if metadata.is_dir() {
                paths.extend(self.enumerate_paths(Some((host_path, metadata)), hasher)?);
            } else {
                paths.push(self.path_with_metadata(host_path, &metadata, hasher)?);
            }
        }

        // Include empty or special dir
        //
        // Regular 755 dir w/ entries can be
        // recreated when adding children
        if let Some((dir, meta)) = subdir {
            const REGULAR_DIR_MODE: u32 = S_IFDIR | S_IROTH | S_IXOTH | S_IRGRP | S_IXGRP | S_IRWXU;

            let is_special = meta.mode() != REGULAR_DIR_MODE;

            if meta.is_dir() && (paths.is_empty() || is_special) {
                paths.push(self.path_with_metadata(dir, &meta, hasher)?);
            }
        }

        Ok(paths)
    }
}

#[derive(Debug)]
pub struct PathInfo {
    pub path: PathBuf,
    pub target_path: PathBuf,
    pub layout: StonePayloadLayoutRecord,
    pub size: u64,
    pub package: String,
}

impl PathInfo {
    pub fn new(
        path: PathBuf,
        target_path: PathBuf,
        metadata: &Metadata,
        hasher: &mut StoneDigestWriterHasher,
        package: String,
    ) -> Result<Self, Error> {
        let layout = layout_from_metadata(&path, &target_path, metadata, hasher)?;

        Ok(Self {
            path,
            target_path,
            layout,
            size: metadata.size(),
            package,
        })
    }

    pub fn restat(&mut self, hasher: &mut StoneDigestWriterHasher) -> Result<(), Error> {
        let metadata = fs::symlink_metadata(&self.path).context(IoSnafu)?;
        self.layout = layout_from_metadata(&self.path, &self.target_path, &metadata, hasher)?;
        self.size = metadata.size();
        Ok(())
    }

    pub fn is_file(&self) -> bool {
        matches!(self.layout.file, StonePayloadLayoutFile::Regular(..))
    }

    pub fn file_hash(&self) -> Option<u128> {
        if let StonePayloadLayoutFile::Regular(hash, _) = &self.layout.file {
            Some(*hash)
        } else {
            None
        }
    }

    pub fn file_name(&self) -> &str {
        self.target_path
            .file_name()
            .and_then(|p| p.to_str())
            .unwrap_or_default()
    }

    pub fn has_component(&self, component: &str) -> bool {
        self.target_path
            .components()
            .any(|c| c.as_os_str() == OsStr::new(component))
    }
}

fn layout_from_metadata(
    path: &Path,
    target_path: &Path,
    metadata: &Metadata,
    hasher: &mut StoneDigestWriterHasher,
) -> Result<StonePayloadLayoutRecord, Error> {
    // Strip /usr
    let target: AStr = target_path
        .strip_prefix("/usr")
        .unwrap_or(target_path)
        .to_string_lossy()
        .into();

    let file_type = metadata.file_type();

    Ok(StonePayloadLayoutRecord {
        uid: metadata.uid(),
        gid: metadata.gid(),
        mode: metadata.mode(),
        tag: 0,
        file: if file_type.is_symlink() {
            let source = fs::read_link(path).context(IoSnafu)?;

            StonePayloadLayoutFile::Symlink(source.to_string_lossy().into(), target)
        } else if file_type.is_dir() {
            StonePayloadLayoutFile::Directory(target)
        } else if file_type.is_char_device() {
            StonePayloadLayoutFile::CharacterDevice(target)
        } else if file_type.is_block_device() {
            StonePayloadLayoutFile::BlockDevice(target)
        } else if file_type.is_fifo() {
            StonePayloadLayoutFile::Fifo(target)
        } else if file_type.is_socket() {
            StonePayloadLayoutFile::Socket(target)
        } else {
            hasher.reset();

            let mut digest_writer = StoneDigestWriter::new(io::sink(), hasher);
            let mut file = fs::File::open(path).context(IoSnafu)?;

            // Copy bytes to null sink so we don't
            // explode memory
            io::copy(&mut file, &mut digest_writer).context(IoSnafu)?;

            let hash = hasher.digest128();

            StonePayloadLayoutFile::Regular(hash, target)
        },
    })
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("no matching path rule"))]
    NoMatchingRule,
    #[snafu(display("io"))]
    Io { source: io::Error },
}

#[cfg(test)]
mod tests {
    use std::{
        fs::Permissions,
        os::unix::{
            fs::{PermissionsExt, symlink},
            net::UnixListener,
        },
        path::Path,
    };

    use super::*;

    fn add_rule(collector: &mut Collector, pattern: &str, package: &str, kind: PathRuleKind) {
        collector.add_rule(Rule {
            pattern: pattern.to_owned(),
            package: package.to_owned(),
            kind,
        });
    }

    fn write_file(root: &Path, relative: &str, mode: u32) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"payload").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn raw_glob_candidates_and_reverse_rule_precedence_select_the_output() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "usr/share/[literal]", 0o644);
        let mut collector = Collector::new(root.path());
        add_rule(&mut collector, "*", "fallback", PathRuleKind::Any);
        add_rule(
            &mut collector,
            &Pattern::escape("/usr/share/[literal]"),
            "lower-priority",
            PathRuleKind::Any,
        );
        add_rule(
            &mut collector,
            &Pattern::escape("/usr/share/[literal]"),
            "highest-priority",
            PathRuleKind::Any,
        );

        let info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();

        assert_eq!(info.target_path, Path::new("/usr/share/[literal]"));
        assert_eq!(info.package, "highest-priority");
    }

    #[test]
    fn executable_rules_require_a_regular_file_with_an_execute_bit() {
        let root = tempfile::tempdir().unwrap();
        let executable = write_file(root.path(), "usr/bin/tool", 0o751);
        let regular = write_file(root.path(), "usr/bin/data", 0o644);
        let mut collector = Collector::new(root.path());
        add_rule(&mut collector, "*", "out", PathRuleKind::Any);
        add_rule(&mut collector, "/usr/bin/*", "executables", PathRuleKind::Executable);
        let mut hasher = StoneDigestWriterHasher::new();

        assert_eq!(collector.path(&executable, &mut hasher).unwrap().package, "executables");
        assert_eq!(collector.path(&regular, &mut hasher).unwrap().package, "out");
    }

    #[test]
    fn symlink_rules_use_lstat_and_enumeration_does_not_follow_linked_directories() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        write_file(external.path(), "nested/file", 0o644);
        let linked_dir = root.path().join("linked-dir");
        symlink(external.path().join("nested"), &linked_dir).unwrap();
        let broken = root.path().join("broken");
        symlink(root.path().join("missing"), &broken).unwrap();

        let mut collector = Collector::new(root.path());
        add_rule(&mut collector, "*", "out", PathRuleKind::Any);
        add_rule(&mut collector, "/*", "links", PathRuleKind::Symlink);
        let paths = collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();

        assert_eq!(
            paths.iter().map(|path| path.target_path.as_path()).collect::<Vec<_>>(),
            [Path::new("/broken"), Path::new("/linked-dir")]
        );
        assert!(paths.iter().all(|path| path.package == "links"));
        assert!(
            paths
                .iter()
                .all(|path| matches!(path.layout.file, StonePayloadLayoutFile::Symlink(..)))
        );
        assert!(paths.iter().all(|path| !path.target_path.ends_with("file")));
    }

    #[test]
    fn special_rules_match_unix_domain_sockets() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("run/service.sock");
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let _listener = UnixListener::bind(&socket).unwrap();
        let mut collector = Collector::new(root.path());
        add_rule(&mut collector, "*", "out", PathRuleKind::Any);
        add_rule(&mut collector, "/run/*", "special", PathRuleKind::Special);

        let info = collector.path(&socket, &mut StoneDigestWriterHasher::new()).unwrap();

        assert_eq!(info.package, "special");
        assert!(matches!(info.layout.file, StonePayloadLayoutFile::Socket(..)));
    }

    #[test]
    fn collection_has_no_implicit_fallback_output() {
        let root = tempfile::tempdir().unwrap();
        let regular = write_file(root.path(), "usr/bin/data", 0o644);
        let mut collector = Collector::new(root.path());
        add_rule(&mut collector, "/usr/bin/*", "executables", PathRuleKind::Executable);

        assert!(matches!(
            collector.path(&regular, &mut StoneDigestWriterHasher::new()),
            Err(Error::NoMatchingRule)
        ));
    }
}
