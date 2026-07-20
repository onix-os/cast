//! Descriptor-rooted discovery of packaged transaction and system triggers.

use std::{fs::File, io, os::fd::AsRawFd as _, path::Path};

use config::GluonCodec;
use gluon_config::Evaluator;
use itertools::Itertools as _;
use triggers::format::Trigger;

use super::{Error, SystemTriggerCodec, TRIGGER_RELATIVE_TO_USR, TransactionTriggerCodec};

pub(super) fn load_transaction(candidate_usr: &File, candidate_usr_path: &Path) -> Result<Vec<Trigger>, Error> {
    load(candidate_usr, candidate_usr_path, &TransactionTriggerCodec, |trigger| {
        trigger.0
    })
}

pub(super) fn load_system(candidate_usr: &File, candidate_usr_path: &Path) -> Result<Vec<Trigger>, Error> {
    load(candidate_usr, candidate_usr_path, &SystemTriggerCodec, |trigger| {
        trigger.0
    })
}

fn load<C: GluonCodec>(
    candidate_usr: &File,
    candidate_usr_path: &Path,
    codec: &C,
    unwrap: impl Fn(C::Config) -> Trigger,
) -> Result<Vec<Trigger>, Error> {
    let trigger_root_path = candidate_usr_path.join(TRIGGER_RELATIVE_TO_USR);
    let trigger_root = match crate::linux_fs::openat2_file(
        candidate_usr.as_raw_fd(),
        c"share/cast/triggers",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        crate::linux_fs::controlled_resolution(),
    ) {
        Ok(root) => root,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error::Config(Box::new(config::LoadGluonError::Enumerate {
                path: trigger_root_path,
                source,
            })));
        }
    };

    config::load_gluon_rooted(&trigger_root_path, &trigger_root, &Evaluator::default(), codec)
        .map_err(|error| Error::Config(Box::new(error)))
        .map(|loaded| loaded.into_iter().map(|loaded| unwrap(loaded.value)).collect_vec())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::OpenOptionsExt as _;

    use itertools::Itertools as _;

    use super::*;

    #[test]
    fn transaction_codec_loads_from_retained_descriptor_after_public_path_substitution() {
        let fixture = substituted_usr_fixture("tx", "original-transaction", "injected-transaction");

        let loaded = load_transaction(&fixture.candidate_usr, &fixture.candidate_usr_path).unwrap();

        assert_eq!(
            loaded.iter().map(|trigger| trigger.name.as_str()).collect_vec(),
            ["original-transaction"]
        );
        assert!(fixture.injected.exists());
        assert!(!loaded.iter().any(|trigger| trigger.name == "injected-transaction"));
    }

    #[test]
    fn system_codec_loads_from_retained_descriptor_after_public_path_substitution() {
        let fixture = substituted_usr_fixture("sys", "original-system", "injected-system");

        let loaded = load_system(&fixture.candidate_usr, &fixture.candidate_usr_path).unwrap();

        assert_eq!(
            loaded.iter().map(|trigger| trigger.name.as_str()).collect_vec(),
            ["original-system"]
        );
        assert!(fixture.injected.exists());
        assert!(!loaded.iter().any(|trigger| trigger.name == "injected-system"));
    }

    #[test]
    fn stateful_system_scope_compiles_intent_from_the_retained_live_usr() {
        let temporary = tempfile::tempdir().unwrap();
        crate::test_support::prepare_private_installation_root(temporary.path());
        let installation = crate::Installation::open(temporary.path(), None).unwrap();
        let local_etc = crate::client::transaction_root::prepare_local_etc(&installation).unwrap();
        let isolation_root = crate::client::create_root_links(&installation.isolation_dir()).unwrap();
        let live_usr_path = installation.root.join("usr");
        let original = live_usr_path.join("share/cast/triggers/sys.d/system.glu");
        fs_err::create_dir_all(original.parent().unwrap()).unwrap();
        fs_err::write(
            &original,
            trigger_source_with_path("original-system", "/bin/true", "system-scope-witness"),
        )
        .unwrap();
        let retained_usr = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
            .open(&live_usr_path)
            .unwrap();

        let displaced = installation.root.join("displaced-system-usr");
        fs_err::rename(&live_usr_path, &displaced).unwrap();
        let injected = live_usr_path.join("share/cast/triggers/sys.d/system.glu");
        fs_err::create_dir_all(injected.parent().unwrap()).unwrap();
        fs_err::write(
            &injected,
            trigger_source_with_path("injected-system", "/bin/false", "system-scope-witness"),
        )
        .unwrap();

        let tree = crate::client::vfs(vec![(
            crate::package::Id::from("retained-system-scope"),
            stone::StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: 0o755,
                tag: 0,
                file: stone::StonePayloadLayoutFile::Directory("share/system-scope-witness".into()),
            },
        )])
        .unwrap();
        let runners = crate::client::postblit::triggers(
            crate::client::postblit::TriggerScope::System {
                installation: &installation,
                isolation_root: &isolation_root,
                local_etc: &local_etc,
                retained_usr: &retained_usr,
                live_usr_path: &live_usr_path,
            },
            &tree,
        )
        .unwrap();

        assert_eq!(runners.len(), 1);
        assert!(matches!(
            runners[0].handler(),
            triggers::format::Handler::Run { run, args } if run == "/bin/true" && args.is_empty()
        ));
        assert!(injected.exists());
        assert!(displaced.join("share/cast/triggers/sys.d/system.glu").exists());
    }

    struct SubstitutedUsrFixture {
        _temporary: tempfile::TempDir,
        candidate_usr: File,
        candidate_usr_path: std::path::PathBuf,
        injected: std::path::PathBuf,
    }

    fn substituted_usr_fixture(domain: &str, original_name: &str, injected_name: &str) -> SubstitutedUsrFixture {
        let temporary = tempfile::tempdir().unwrap();
        let staging = temporary.path().join("staging");
        let candidate_usr_path = staging.join("usr");
        let original = candidate_usr_path.join(format!("share/cast/triggers/{domain}.d/original.glu"));
        fs_err::create_dir_all(original.parent().unwrap()).unwrap();
        fs_err::write(&original, trigger_source(original_name, "/bin/true")).unwrap();
        let candidate_usr = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
            .open(&candidate_usr_path)
            .unwrap();

        let displaced = temporary.path().join("displaced-staging");
        fs_err::rename(&staging, &displaced).unwrap();
        let injected = candidate_usr_path.join(format!("share/cast/triggers/{domain}.d/injected.glu"));
        fs_err::create_dir_all(injected.parent().unwrap()).unwrap();
        fs_err::write(&injected, trigger_source(injected_name, "/bin/false")).unwrap();

        SubstitutedUsrFixture {
            _temporary: temporary,
            candidate_usr,
            candidate_usr_path,
            injected,
        }
    }

    fn trigger_source(name: &str, command: &str) -> String {
        trigger_source_with_path(name, command, name)
    }

    fn trigger_source_with_path(name: &str, command: &str, witness: &str) -> String {
        format!(
            r#"let cast = import! cast.trigger.v1
let base = cast.trigger "{name}" "Retained trigger discovery fixture"
{{
    paths = [cast.path
        "/usr/share/{witness}"
        ["{name}"]
        (cast.optional.set cast.path_kind.directory)],
    handlers = [cast.handler.named "{name}" (cast.handler.run
        "{command}"
        [])],
    .. base
}}
"#
        )
    }
}
