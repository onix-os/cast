// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Git repository manipulation utilities based
//! on the `git` executable.
//!
//! For any operation, `git` is called under the hood:
//! make sure it is available in your `$PATH`, otherwise
//! [Error] will be returned.
//!
//! Even though we are aware that calling executables is brittle API,
//! neither libgit2 nor gitoxide had all operations available in this
//! module implemented.

use std::env;
use std::ffi::OsStr;
use std::path::{self, Path, PathBuf};
use std::process::Stdio;

use tokio::{io, process};
use url::Url;

pub mod error;
pub use self::error::Error;
use error::{Constraint, InnerError};

/// An uninitialized repository, useful for unit tests.
pub fn null_repository() -> Repository {
    Repository { path: PathBuf::new() }
}

/// A Git repository.
pub struct Repository {
    path: PathBuf,
}

impl Repository {
    /// Opens a local bare Git repository.
    /// If the Git repository at `path` is not bare,
    /// an [Error] containing [Constraint::NotBare] is returned.
    pub async fn open_bare(path: &Path) -> Result<Self, Error> {
        let path = path::absolute(path).map_err(InnerError::from)?;
        let output = run_git(&[
            OsStr::new("-C"),
            path.as_os_str(),
            OsStr::new("repo"),
            OsStr::new("info"),
            OsStr::new("layout.bare"),
        ])
        .await?;
        if !output.stdout.starts_with(b"layout.bare=true") {
            return Err(InnerError::Constraint(Constraint::NotBare))?;
        }
        Ok(Self { path })
    }

    /// Clones a local or remote Git repository as bare into `path`.
    /// The clone is performed with Git's `--mirror` flag.
    pub async fn clone_mirror(path: &Path, url: &Url) -> Result<Self, Error> {
        let path = path::absolute(path).map_err(InnerError::from)?;
        run_git(&[
            OsStr::new("clone"),
            OsStr::new("--mirror"),
            OsStr::new("--no-hardlinks"),
            OsStr::new("--no-recurse-submodules"),
            OsStr::new(&url.as_str()),
            path.as_os_str(),
        ])
        .await?;
        Ok(Self { path })
    }

    /// Clones a local or remote Git repository as bare into `path`.
    /// The clone is performed with Git's `--mirror` flag.
    /// A callback is fired repeatedly to track the cloning
    /// process in real time.
    pub async fn clone_mirror_progress<F>(path: &Path, url: &Url, callback: F) -> Result<Self, Error>
    where
        F: Fn(FetchProgress),
    {
        let path = path::absolute(path).map_err(InnerError::from)?;
        run_git_progress(
            &[
                OsStr::new("clone"),
                OsStr::new("--mirror"),
                OsStr::new("--no-hardlinks"),
                OsStr::new("--no-recurse-submodules"),
                OsStr::new("--progress"),
                OsStr::new(&url.as_str()),
                path.as_os_str(),
            ],
            callback,
        )
        .await?;
        Ok(Self { path })
    }

    /// Whether this repository has a commit identified by its hash.
    pub async fn has_commit(&self, commit: &str) -> Result<bool, Error> {
        let output = run_git(&[
            OsStr::new("-C"),
            self.path.as_os_str(),
            OsStr::new("cat-file"),
            OsStr::new("-t"),
            OsStr::new(commit),
        ])
        .await?;
        Ok(output.stdout.starts_with(b"commit"))
    }

    /// Returns the hash of the commit. If a commit hash is passed,
    /// the output is equal to `commit`. If a Git reference is passed, the
    /// reference is peeled through annotated tags into the commit object.
    pub async fn peel_commit(&self, commit: &str) -> Result<String, Error> {
        let commit = format!("{commit}^{{commit}}");
        let output = run_git(&[
            OsStr::new("-C"),
            self.path.as_os_str(),
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new("--end-of-options"),
            OsStr::new(&commit),
        ])
        .await?;
        let object_id = str::from_utf8(output.stdout.trim_ascii_end()).unwrap_or("");
        if object_id.len() != 40
            || !object_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(InnerError::Run {
                code: None,
                stderr: Some("Git returned a commit object ID that is not a lowercase 40-hex SHA-1".to_owned()),
            })?;
        }
        Ok(object_id.to_owned())
    }

    /// Returns the remote URL for the provided `remote`
    pub async fn get_remote_url(&self, remote: &str) -> Result<String, Error> {
        let output = run_git(&[
            OsStr::new("-C"),
            self.path.as_os_str(),
            OsStr::new("remote"),
            OsStr::new("get-url"),
            OsStr::new(remote),
        ])
        .await?;
        Ok(str::from_utf8(output.stdout.trim_ascii_end()).unwrap_or("").to_owned())
    }

    /// Sets the remote URL for the provided `remote` to `url`
    pub async fn set_remote_url(&self, remote: &str, url: &str) -> Result<(), Error> {
        run_git(&[
            OsStr::new("-C"),
            self.path.as_os_str(),
            OsStr::new("remote"),
            OsStr::new("set-url"),
            OsStr::new(remote),
            OsStr::new(url),
        ])
        .await?;
        Ok(())
    }

    /// Checkout the provided `rev` (branch or commit)
    pub async fn checkout(&self, rev: &str) -> Result<(), Error> {
        run_git(&[
            OsStr::new("-C"),
            self.path.as_os_str(),
            OsStr::new("checkout"),
            OsStr::new("--detach"),
            OsStr::new("--force"),
            OsStr::new("--no-recurse-submodules"),
            OsStr::new(rev),
        ])
        .await?;
        Ok(())
    }

    /// Equivalent to `git fetch`.
    /// A callback is fired repeatedly to track the fetching
    /// process in real time.
    pub async fn fetch_progress<F>(&self, callback: F) -> Result<(), Error>
    where
        F: Fn(FetchProgress),
    {
        run_git_progress(
            &[
                OsStr::new("-C"),
                self.path.as_os_str(),
                OsStr::new("fetch"),
                OsStr::new("--no-recurse-submodules"),
                OsStr::new("--progress"),
            ],
            callback,
        )
        .await?;
        Ok(())
    }

    /// Clone the current [`Repository`] to the provided `path` and return
    /// the cloned to [`Repository`].
    pub async fn clone_to(&self, path: &Path) -> Result<Self, Error> {
        let path = path::absolute(path).map_err(InnerError::from)?;

        // Clone it to `path`
        run_git(&[
            OsStr::new("clone"),
            OsStr::new("--no-hardlinks"),
            OsStr::new("--no-recurse-submodules"),
            self.path.as_os_str(),
            path.as_os_str(),
        ])
        .await?;

        Ok(Self { path: path.to_owned() })
    }

    /// Whether `rev` contains Gitlink entries whose contents would require
    /// an additional, independently fetched submodule source graph.
    pub async fn contains_gitlinks(&self, rev: &str) -> Result<bool, Error> {
        let output = run_git(&[
            OsStr::new("-C"),
            self.path.as_os_str(),
            OsStr::new("ls-tree"),
            OsStr::new("-r"),
            OsStr::new("--full-tree"),
            OsStr::new(rev),
        ])
        .await?;
        Ok(output
            .stdout
            .split(|byte| *byte == b'\n')
            .any(|line| line.starts_with(b"160000 ")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The argument of callbacks when they are invoked
/// for reporting a Git operation's progress.
pub struct FetchProgress {
    /// Completion percentage.
    pub percent: u8,
    /// Download speed in formatted human units per second
    pub speed: String,
}

/// Runs git and waits for it to terminate.
async fn run_git<I, S>(args: I) -> Result<std::process::Output, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git_command()
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(InnerError::from)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(InnerError::Run {
            code: output.status.code(),
            stderr: Some(String::from_utf8(output.stderr).unwrap()),
        })?
    }
}

async fn run_git_progress<I, S, F>(args: I, callback: F) -> Result<(), Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
    F: Fn(FetchProgress),
{
    let (mut git, stderr) = spawn_git(args)?;

    let parser = async move {
        let prog = ProgressParser::new(stderr);
        prog.parse(callback).await
    };

    let (_, result) = tokio::join!(parser, git.wait());
    let result = result.map_err(InnerError::from)?;
    if result.success() {
        Ok(())
    } else {
        Err(InnerError::Run {
            code: result.code(),
            stderr: None,
        })?
    }
}

fn spawn_git<I, S>(args: I) -> Result<(process::Child, process::ChildStderr), Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = git_command()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(InnerError::from)?;
    let stderr = child.stderr.take().unwrap();
    Ok((child, stderr))
}

/// Construct Git with a deliberately small, stable process environment.
///
/// Source transport may change whether a fetch succeeds, but it must not
/// activate user/system configuration, credential helpers, hooks, filters, or
/// locale-dependent checkout behavior that can change locked source bytes.
fn git_command() -> process::Command {
    let path = env::var_os("PATH");
    let mut command = process::Command::new("git");
    command.env_clear();
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command
        .env("HOME", "/nonexistent")
        .env("XDG_CONFIG_HOME", "/nonexistent")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.symlinks=true",
        ]);
    command
}

struct ProgressParser<R: io::AsyncRead> {
    reader: io::BufReader<R>,
}

impl<R: io::AsyncRead + Unpin> ProgressParser<R> {
    const TERMINATOR: u8 = b'\r';
    const PREFIX: &[u8] = b"Receiving objects:";

    pub fn new(stderr: R) -> Self {
        Self {
            reader: io::BufReader::new(stderr),
        }
    }

    // We're parsing lines like:
    // "Receiving objects:  26% (163045/627093), 52.57 MiB | 34.99 MiB/s"
    // And we want the percentage and the speed, which are conveniently
    // the first and the last tokens of the line.

    pub async fn parse(self, callback: impl Fn(FetchProgress)) -> Result<(), Error> {
        use tokio::io::AsyncBufReadExt;

        let mut lines = self.reader.split(Self::TERMINATOR);
        while let Some(line) = lines.next_segment().await.map_err(InnerError::from)? {
            if !line.starts_with(Self::PREFIX) {
                continue;
            }
            let line = &str::from_utf8(&line[Self::PREFIX.len()..]).unwrap_or("");
            if let Some(progress) = Self::parse_progress(line) {
                callback(progress);
            }
        }
        Ok(())
    }

    fn parse_progress(line: &str) -> Option<FetchProgress> {
        let mut tokens = line.split_ascii_whitespace();

        let percent = tokens.next()?;
        let unit_per_sec = tokens.next_back()?;
        let speed = tokens.next_back()?;

        if !unit_per_sec.ends_with("/s") {
            return None;
        }

        Some(FetchProgress {
            percent: percent.strip_suffix('%')?.parse().ok()?,
            speed: format!("{speed} {unit_per_sec}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, process::Command};

    use super::*;

    fn fixture_git(repository: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[tokio::test]
    async fn gitlinks_are_detected_without_materializing_submodules() {
        let temporary = tempfile::tempdir().unwrap();
        let repository_path = temporary.path().join("repository");
        std::fs::create_dir(&repository_path).unwrap();
        fixture_git(&repository_path, &["init", "--initial-branch=main"]);
        fixture_git(&repository_path, &["config", "user.name", "Gitwrap Test"]);
        fixture_git(&repository_path, &["config", "user.email", "gitwrap@example.invalid"]);
        std::fs::write(repository_path.join("source.txt"), b"locked source\n").unwrap();
        fixture_git(&repository_path, &["add", "source.txt"]);
        fixture_git(&repository_path, &["commit", "-m", "source"]);

        let repository = Repository {
            path: repository_path.clone(),
        };
        let source_commit = fixture_git(&repository_path, &["rev-parse", "HEAD"]);
        assert!(!repository.contains_gitlinks(&source_commit).await.unwrap());

        let cache_info = format!("160000,{source_commit},vendor/dependency");
        fixture_git(&repository_path, &["update-index", "--add", "--cacheinfo", &cache_info]);
        fixture_git(&repository_path, &["commit", "-m", "gitlink"]);
        let gitlink_commit = fixture_git(&repository_path, &["rev-parse", "HEAD"]);

        assert!(repository.contains_gitlinks(&gitlink_commit).await.unwrap());
    }

    #[tokio::test]
    async fn annotated_tags_are_peeled_to_the_commit_object() {
        let temporary = tempfile::tempdir().unwrap();
        let repository_path = temporary.path().join("repository");
        std::fs::create_dir(&repository_path).unwrap();
        fixture_git(&repository_path, &["init", "--initial-branch=main"]);
        fixture_git(&repository_path, &["config", "user.name", "Gitwrap Test"]);
        fixture_git(&repository_path, &["config", "user.email", "gitwrap@example.invalid"]);
        std::fs::write(repository_path.join("source.txt"), b"locked source\n").unwrap();
        fixture_git(&repository_path, &["add", "source.txt"]);
        fixture_git(&repository_path, &["commit", "-m", "source"]);
        fixture_git(
            &repository_path,
            &["tag", "--annotate", "v1", "--message", "release v1"],
        );

        let commit = fixture_git(&repository_path, &["rev-parse", "HEAD"]);
        let tag_object = fixture_git(&repository_path, &["rev-parse", "v1"]);
        assert_ne!(tag_object, commit, "the fixture must use an annotated tag object");

        let repository = Repository { path: repository_path };
        let peeled = repository.peel_commit("v1").await.unwrap();

        assert_eq!(peeled, commit);
        assert_eq!(peeled.len(), 40);
        assert!(peeled.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
    }
}
