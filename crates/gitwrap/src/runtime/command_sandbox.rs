/// Construct Git with a deliberately small, stable process environment.
///
/// Source transport may change whether a fetch succeeds, but it must not
/// activate user/system configuration, credential helpers, hooks, filters, or
/// locale-dependent checkout behavior that can change locked source bytes.
fn git_command(limits: Limits) -> process::Command {
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
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        // Ignore SSH configuration capable of launching ProxyCommand or local
        // commands. `ssh` itself is resolved from the same trusted PATH as Git;
        // unknown Git remote-helper transports are rejected before spawn.
        .env(
            "GIT_SSH_COMMAND",
            "ssh -F /dev/null -oBatchMode=yes -oPermitLocalCommand=no -oProxyCommand=none",
        )
        .env("GIT_SSH_VARIANT", "ssh")
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.symlinks=true",
            "-c",
            "credential.helper=",
            "-c",
            "credential.useHttpPath=true",
            "-c",
            "fetch.recurseSubmodules=false",
            "-c",
            "http.cookieFile=",
            "-c",
            "http.extraHeader=",
            "-c",
            "http.proxy=",
            "-c",
            "http.sslVerify=true",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "protocol.http.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "protocol.ssh.allow=always",
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "remote.origin.proxy=",
            "-c",
            "remote.origin.uploadpack=git-upload-pack",
            "-c",
            "submodule.recurse=false",
        ]);
    constrain_process(&mut command, limits);
    command
}

fn set_command_directory(command: &mut process::Command, directory: &fs::File) {
    let descriptor = directory.as_raw_fd();
    // The descriptor itself remains close-on-exec. fchdir pins the child cwd to
    // the already-validated inode before Git starts, so a concurrent rename or
    // symlink replacement of the caller-visible path cannot redirect it.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if nix::libc::fchdir(descriptor) == -1 {
                Err(std_io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

fn constrain_process(command: &mut process::Command, limits: Limits) {
    // The process group contains Git plus transport helpers such as ssh. The
    // per-file RLIMIT_FSIZE is an OS-enforced backstop complementing the
    // monitored aggregate repository quota.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if nix::libc::setpgid(0, 0) == -1 {
                return Err(std_io::Error::last_os_error());
            }
            let core = nix::libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if nix::libc::setrlimit(nix::libc::RLIMIT_CORE, &core) == -1 {
                return Err(std_io::Error::last_os_error());
            }

            let mut inherited_nofile = nix::libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut inherited_nofile) == -1 {
                return Err(std_io::Error::last_os_error());
            }
            let requested_nofile = rlim_from_u64(limits.open_files);
            let nofile_max = inherited_nofile.rlim_max.min(requested_nofile);
            let nofile = nix::libc::rlimit {
                rlim_cur: inherited_nofile.rlim_cur.min(nofile_max),
                rlim_max: nofile_max,
            };
            if nix::libc::setrlimit(nix::libc::RLIMIT_NOFILE, &nofile) == -1 {
                return Err(std_io::Error::last_os_error());
            }

            let mut inherited_address_space = nix::libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if nix::libc::getrlimit(nix::libc::RLIMIT_AS, &mut inherited_address_space) == -1 {
                return Err(std_io::Error::last_os_error());
            }
            let address_space_max = inherited_address_space
                .rlim_max
                .min(rlim_from_u64(limits.address_space_bytes));
            let address_space = nix::libc::rlimit {
                rlim_cur: inherited_address_space.rlim_cur.min(address_space_max),
                rlim_max: address_space_max,
            };
            if nix::libc::setrlimit(nix::libc::RLIMIT_AS, &address_space) == -1 {
                return Err(std_io::Error::last_os_error());
            }

            let mut current = nix::libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if nix::libc::getrlimit(nix::libc::RLIMIT_FSIZE, &mut current) == -1 {
                return Err(std_io::Error::last_os_error());
            }
            let requested = rlim_from_u64(limits.repository_bytes);
            current.rlim_cur = current.rlim_cur.min(requested);
            current.rlim_max = current.rlim_max.min(requested);
            if nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &current) == -1 {
                return Err(std_io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(target_pointer_width = "64")]
fn rlim_from_u64(value: u64) -> nix::libc::rlim_t {
    value
}

#[cfg(not(target_pointer_width = "64"))]
fn rlim_from_u64(value: u64) -> nix::libc::rlim_t {
    nix::libc::rlim_t::try_from(value).unwrap_or(nix::libc::rlim_t::MAX)
}

struct ProgressParser<R: io::AsyncRead> {
    reader: R,
    total_limit: usize,
    segment_limit: usize,
}

impl<R: io::AsyncRead + Unpin> ProgressParser<R> {
    const PREFIX: &[u8] = b"Receiving objects:";

    pub fn new(stderr: R, total_limit: usize, segment_limit: usize) -> Self {
        Self {
            reader: stderr,
            total_limit,
            segment_limit,
        }
    }

    // We're parsing lines like:
    // "Receiving objects:  26% (163045/627093), 52.57 MiB | 34.99 MiB/s"
    // And we want the percentage and the speed, which are conveniently
    // the first and the last tokens of the line.

    pub async fn parse(mut self, callback: impl Fn(FetchProgress)) -> Result<(), Error> {
        let mut total = 0_usize;
        let mut segment = Vec::with_capacity(self.segment_limit.min(1024));
        let mut chunk = [0_u8; 8192];
        loop {
            let count = self.reader.read(&mut chunk).await.map_err(InnerError::from)?;
            if count == 0 {
                Self::report_segment(&segment, &callback);
                return Ok(());
            }
            if count > self.total_limit.saturating_sub(total) {
                return Err(InnerError::OutputLimit {
                    stream: "stderr",
                    limit: self.total_limit,
                }
                .into());
            }
            total += count;
            for byte in &chunk[..count] {
                if matches!(*byte, b'\r' | b'\n') {
                    Self::report_segment(&segment, &callback);
                    segment.clear();
                } else if segment.len() == self.segment_limit {
                    return Err(InnerError::ProgressSegmentLimit {
                        limit: self.segment_limit,
                    }
                    .into());
                } else {
                    segment.push(*byte);
                }
            }
        }
    }

    fn report_segment(segment: &[u8], callback: &impl Fn(FetchProgress)) {
        if !segment.starts_with(Self::PREFIX) {
            return;
        }
        let line = str::from_utf8(&segment[Self::PREFIX.len()..]).unwrap_or("");
        if let Some(progress) = Self::parse_progress(line) {
            callback(progress);
        }
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
