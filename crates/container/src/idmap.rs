// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Deterministic credential mapping for isolated build containers.
//!
//! A payload starts with namespace UID/GID zero and an empty supplementary
//! group list.  Linux requires `setgroups(2)` to remain enabled until the
//! child has dropped its inherited groups, so an unprivileged parent uses one
//! delegated subordinate GID solely to authorize that transition.  The
//! subordinate host ID is never exposed to the payload: it is always mapped
//! to the fixed namespace-only auxiliary GID below.

use std::{io, path::Path, process::Command};

use fs_err as fs;
use nix::{
    errno::Errno,
    unistd::{Pid, Uid, User, getegid, geteuid, getgid, getuid},
};
use snafu::{ResultExt, Snafu, ensure};

/// A fixed, non-payload namespace GID used to keep `setgroups` enabled while
/// the child clears inherited supplementary groups.
const AUXILIARY_GID: u32 = u32::MAX - 1;
const NEWGIDMAP: &str = "/usr/bin/newgidmap";

pub fn idmap(pid: Pid) -> Result<(), Error> {
    ensure_same_real_and_effective_ids()?;

    let uid = geteuid().as_raw();
    let gid = getegid().as_raw();
    let proc_dir = Path::new("/proc").join(pid.as_raw().to_string());

    fs::write(proc_dir.join("uid_map"), mapping(0, uid)).context(WriteUidMapSnafu)?;

    let privileged_auxiliary_gid = if gid == AUXILIARY_GID {
        AUXILIARY_GID - 1
    } else {
        AUXILIARY_GID
    };
    let direct_map = gid_mapping(gid, privileged_auxiliary_gid);
    match fs::write(proc_dir.join("gid_map"), &direct_map) {
        Ok(()) => {}
        Err(source) if is_permission_denied(&source) => {
            let subordinate_gid = delegated_gid(uid, gid)?;
            map_gid_with_helper(pid, gid, subordinate_gid)?;
        }
        Err(source) => return Err(Error::WriteGidMap { source }),
    }

    verify_map(&proc_dir.join("uid_map"), &[(0, uid, 1)], "UID").context(VerifyUidMapSnafu)?;
    let gid_map = read_map(&proc_dir.join("gid_map")).context(VerifyGidMapSnafu)?;
    ensure!(
        gid_map.len() == 2
            && gid_map[0] == (0, gid, 1)
            && gid_map[1].0 == AUXILIARY_GID
            && gid_map[1].2 == 1
            && gid_map[1].1 != gid,
        UnexpectedGidMapSnafu { found: gid_map }
    );

    let setgroups = fs::read_to_string(proc_dir.join("setgroups")).context(ReadSetgroupsSnafu)?;
    ensure!(setgroups.trim() == "allow", SetgroupsDisabledSnafu);
    Ok(())
}

fn ensure_same_real_and_effective_ids() -> Result<(), Error> {
    let real_uid = getuid();
    let effective_uid = geteuid();
    let real_gid = getgid();
    let effective_gid = getegid();
    ensure!(
        real_uid == effective_uid && real_gid == effective_gid,
        MixedCallerCredentialsSnafu {
            real_uid: real_uid.as_raw(),
            effective_uid: effective_uid.as_raw(),
            real_gid: real_gid.as_raw(),
            effective_gid: effective_gid.as_raw(),
        }
    );
    Ok(())
}

fn mapping(namespace_id: u32, host_id: u32) -> String {
    format!("{namespace_id} {host_id} 1\n")
}

fn gid_mapping(primary_gid: u32, auxiliary_host_gid: u32) -> String {
    format!(
        "{}{}",
        mapping(0, primary_gid),
        mapping(AUXILIARY_GID, auxiliary_host_gid)
    )
}

fn is_permission_denied(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(Errno::EPERM as i32)
}

fn delegated_gid(uid: u32, primary_gid: u32) -> Result<u32, Error> {
    let user = User::from_uid(Uid::from_raw(uid))
        .context(GetUserByUidSnafu)?
        .ok_or(Error::MissingUser { uid })?;
    let content = fs::read_to_string("/etc/subgid").context(ReadSubgidSnafu)?;
    select_delegated_gid(&content, uid, &user.name, primary_gid).ok_or(Error::MissingSubgid {
        uid,
        username: user.name,
    })
}

fn select_delegated_gid(content: &str, uid: u32, username: &str, primary_gid: u32) -> Option<u32> {
    content
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let owner = fields.next()?;
            let start = fields.next()?.parse::<u32>().ok()?;
            let count = fields.next()?.parse::<u32>().ok()?;
            if fields.next().is_some() || (owner != username && owner.parse::<u32>() != Ok(uid)) || count == 0 {
                return None;
            }
            let end = start.checked_add(count)?;
            (start..end).find(|candidate| *candidate != primary_gid)
        })
        .min()
}

fn map_gid_with_helper(pid: Pid, primary_gid: u32, subordinate_gid: u32) -> Result<(), Error> {
    let output = Command::new(NEWGIDMAP)
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .args([
            pid.as_raw().to_string(),
            "0".to_owned(),
            primary_gid.to_string(),
            "1".to_owned(),
            AUXILIARY_GID.to_string(),
            subordinate_gid.to_string(),
            "1".to_owned(),
        ])
        .output()
        .context(RunNewgidmapSnafu)?;
    ensure!(
        output.status.success(),
        NewgidmapFailedSnafu {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }
    );
    Ok(())
}

fn verify_map(path: &Path, expected: &[(u32, u32, u32)], kind: &'static str) -> Result<(), io::Error> {
    let found = read_map(path)?;
    if found == expected {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "unexpected {kind} map {found:?}; expected {expected:?}"
        )))
    }
}

fn read_map(path: &Path) -> Result<Vec<(u32, u32, u32)>, io::Error> {
    fs::read_to_string(path)?
        .lines()
        .map(|line| {
            let fields = line
                .split_ascii_whitespace()
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(io::Error::other)?;
            if let [namespace_id, host_id, count] = fields.as_slice() {
                Ok((*namespace_id, *host_id, *count))
            } else {
                Err(io::Error::other(format!("invalid ID map line {line:?}")))
            }
        })
        .collect()
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display(
        "container mapping requires equal real/effective credentials, found uid {real_uid}/{effective_uid} and gid {real_gid}/{effective_gid}"
    ))]
    MixedCallerCredentials {
        real_uid: u32,
        effective_uid: u32,
        real_gid: u32,
        effective_gid: u32,
    },
    #[snafu(display("write namespace UID map"))]
    WriteUidMap { source: io::Error },
    #[snafu(display("write namespace GID map"))]
    WriteGidMap { source: io::Error },
    #[snafu(display("verify namespace UID map"))]
    VerifyUidMap { source: io::Error },
    #[snafu(display("read namespace GID map"))]
    VerifyGidMap { source: io::Error },
    #[snafu(display("unexpected namespace GID map {found:?}"))]
    UnexpectedGidMap { found: Vec<(u32, u32, u32)> },
    #[snafu(display("read namespace setgroups policy"))]
    ReadSetgroups { source: io::Error },
    #[snafu(display("namespace setgroups policy was disabled before inherited groups could be cleared"))]
    SetgroupsDisabled,
    #[snafu(display("look up caller UID"))]
    GetUserByUid { source: nix::Error },
    #[snafu(display("caller UID {uid} has no passwd entry"))]
    MissingUser { uid: u32 },
    #[snafu(display("read /etc/subgid"))]
    ReadSubgid { source: io::Error },
    #[snafu(display("caller {username} (UID {uid}) needs at least one delegated subordinate GID in /etc/subgid"))]
    MissingSubgid { uid: u32, username: String },
    #[snafu(display("run fixed mapping helper {NEWGIDMAP}"))]
    RunNewgidmap { source: io::Error },
    #[snafu(display("{NEWGIDMAP} failed with {status}: {stderr}"))]
    NewgidmapFailed { status: String, stderr: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings_have_fixed_namespace_identities() {
        assert_eq!(mapping(0, 1001), "0 1001 1\n");
        assert_eq!(
            gid_mapping(1002, 200000),
            format!("0 1002 1\n{AUXILIARY_GID} 200000 1\n")
        );
    }

    #[test]
    fn delegated_gid_selection_is_order_independent_and_uses_one_id() {
        let first = "builder:300000:10\n1001:200000:5\nother:100000:20\n";
        let second = "other:100000:20\n1001:200000:5\nbuilder:300000:10\n";
        assert_eq!(select_delegated_gid(first, 1001, "builder", 1002), Some(200000));
        assert_eq!(select_delegated_gid(second, 1001, "builder", 1002), Some(200000));
    }

    #[test]
    fn delegated_gid_selection_skips_primary_and_malformed_ranges() {
        let content = "builder:1002:2\nbuilder:not-a-gid:4\nbuilder:400000:0\ninvalid\n";
        assert_eq!(select_delegated_gid(content, 1001, "builder", 1002), Some(1003));
        assert_eq!(select_delegated_gid("other:1003:1\n", 1001, "builder", 1002), None);
    }

    #[test]
    fn map_parser_rejects_noncanonical_shapes() {
        let temporary = tempfile::tempdir().unwrap();
        let map = temporary.path().join("map");
        fs::write(&map, "0 1000\n").unwrap();
        assert!(read_map(&map).is_err());
        fs::write(&map, "0 1000 1\n").unwrap();
        assert_eq!(read_map(&map).unwrap(), vec![(0, 1000, 1)]);
    }
}
