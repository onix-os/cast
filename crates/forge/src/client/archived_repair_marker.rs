//! A durable "an archived repair was interrupted" marker.
//!
//! Archived repair rebuilds an *inactive* tree: no live `/usr` mutation, no
//! boot, transaction triggers only. That narrow blast radius is why it does not
//! carry a full transition journal record (`plans/future_impl.md` §1.3, D1.3) —
//! none of the journal's exchange, boot or rollback machinery applies, and a
//! second forward phase chain would cost every phase-driven consumer for no
//! gain.
//!
//! What it *does* need is for the next startup to notice an interrupted repair.
//! The in-process story is already handled: `publish` derives `RepairLayout`
//! from the on-disk namespace and resumes from it, and `finish_complete`
//! performs no database write, so there is no torn in-process commit. The gap is
//! across a restart. Preparation writes the candidate row before the tree is
//! published, so a crash in between leaves a row that can describe a repaired
//! state whose tree was never swapped — and nothing was obliged to detect it.
//!
//! So this marker's whole job is to make that visible. It is armed before the
//! first mutation and disarmed once publication is durable; a marker found at
//! startup means "reconcile this state's row against its namespace".
//!
//! Deliberately not a journal record: it carries one state ID and no phase. The
//! namespace is the source of truth for *where* a repair got to, and
//! `RepairLayout` already reads it.

use std::{
    io::{Read, Write},
    path::PathBuf,
};

use crate::{Installation, state};

/// Marker file name below `.cast`. Sibling of `journal`, deliberately a plain
/// file rather than a locked directory: it is read once at startup and is never
/// a mutual-exclusion authority.
const MARKER_NAME: &str = "archived-repair-pending";

fn marker_path(installation: &Installation) -> PathBuf {
    installation.root.join(".cast").join(MARKER_NAME)
}

/// Record that an archived repair of `state` is about to mutate the namespace.
///
/// Durable before returning: both the file and its parent directory are synced,
/// so a power loss immediately after cannot lose the marker while keeping the
/// mutation it guards.
pub(crate) fn arm(installation: &Installation, state: state::Id) -> Result<(), Error> {
    let path = marker_path(installation);
    let mut file = std::fs::File::create(&path).map_err(|source| Error::Write {
        path: path.clone(),
        source,
    })?;
    file.write_all(i32::from(state).to_string().as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|source| Error::Write {
            path: path.clone(),
            source,
        })?;
    sync_cast_directory(installation)
}

/// Clear the marker once publication is durable.
///
/// Ordering matters and is the caller's responsibility: disarming before the
/// publication is synced would reintroduce exactly the window this exists to
/// close. Missing is not an error — a repair that never armed, or a disarm
/// retried after a partial failure, both land here.
pub(crate) fn disarm(installation: &Installation) -> Result<(), Error> {
    let path = marker_path(installation);
    match std::fs::remove_file(&path) {
        Ok(()) => sync_cast_directory(installation),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Clear { path, source }),
    }
}

/// The state whose archived repair was interrupted, if any.
pub(crate) fn pending(installation: &Installation) -> Result<Option<state::Id>, Error> {
    let path = marker_path(installation);
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::Read { path, source }),
    };
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|source| Error::Read {
        path: path.clone(),
        source,
    })?;
    let raw: i32 = text
        .trim()
        .parse()
        .map_err(|_| Error::Malformed { path: path.clone() })?;
    if raw <= 0 {
        return Err(Error::Malformed { path });
    }
    Ok(Some(state::Id::from(raw)))
}

fn sync_cast_directory(installation: &Installation) -> Result<(), Error> {
    let path = installation.root.join(".cast");
    std::fs::File::open(&path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| Error::Sync { path, source })
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("write the archived-repair marker at {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("read the archived-repair marker at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("clear the archived-repair marker at {path}")]
    Clear {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sync the cast directory at {path}")]
    Sync {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the archived-repair marker at {path} is malformed")]
    Malformed { path: PathBuf },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::private_installation_tempdir;

    fn installation_with_cast() -> (tempfile::TempDir, Installation) {
        let temporary = private_installation_tempdir();
        // Let `Installation::open` create `.cast` itself: it is owner-only by
        // policy, and a plain `create_dir_all` here would inherit the umask and
        // be rejected as an unsafe capability root.
        let installation = Installation::open(temporary.path().to_owned(), None).unwrap();
        (temporary, installation)
    }

    #[test]
    fn an_armed_marker_is_readable_and_disarming_clears_it() {
        let (_temporary, installation) = installation_with_cast();
        assert_eq!(pending(&installation).unwrap(), None, "nothing armed yet");

        arm(&installation, state::Id::from(42)).unwrap();
        assert_eq!(
            pending(&installation).unwrap(),
            Some(state::Id::from(42)),
            "an armed marker names the state under repair",
        );

        // Arming is idempotent in the sense that matters: a retried repair
        // overwrites rather than accumulating markers.
        arm(&installation, state::Id::from(43)).unwrap();
        assert_eq!(pending(&installation).unwrap(), Some(state::Id::from(43)));

        disarm(&installation).unwrap();
        assert_eq!(pending(&installation).unwrap(), None, "disarming clears it");

        // A repair that never armed, or a disarm retried after a partial
        // failure, must not turn into an error.
        disarm(&installation).unwrap();
    }

    #[test]
    fn a_malformed_marker_is_never_read_as_a_state() {
        let (_temporary, installation) = installation_with_cast();
        let path = marker_path(&installation);

        for corrupt in ["", "not-a-number", "0", "-7"] {
            std::fs::write(&path, corrupt).unwrap();
            assert!(
                matches!(pending(&installation), Err(Error::Malformed { .. })),
                "{corrupt:?} must not decode as a state identifier",
            );
        }
    }
}
