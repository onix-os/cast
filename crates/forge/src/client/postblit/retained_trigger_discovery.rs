//! Descriptor-rooted discovery of packaged transaction and system triggers.

use std::{fs::File, io, os::fd::AsRawFd as _, path::Path};

use itertools::Itertools as _;
use triggers::format::Trigger;

use super::{
    Error, TRIGGER_RELATIVE_TO_USR,
    trigger_declaration::{self, SystemTrigger, TransactionTrigger},
};

pub(super) fn load_transaction(candidate_usr: &File, candidate_usr_path: &Path) -> Result<Vec<Trigger>, Error> {
    let (trigger_root_path, trigger_root) = open_trigger_root(candidate_usr, candidate_usr_path)?;
    let Some(trigger_root) = trigger_root else {
        return Ok(Vec::new());
    };
    let evaluators = trigger_declaration::transaction_evaluators();
    config::declaration::load_rooted_declarations(
        &trigger_root_path,
        &trigger_root,
        &evaluators,
    )
    .map_err(|source| Error::RootedTriggerDeclarations {
        source: Box::new(source),
    })
    .map(|loaded| {
        loaded
            .into_iter()
            .map(|loaded| {
                let TransactionTrigger(trigger) = loaded.value;
                trigger
            })
            .collect_vec()
    })
}

pub(super) fn load_system(candidate_usr: &File, candidate_usr_path: &Path) -> Result<Vec<Trigger>, Error> {
    let (trigger_root_path, trigger_root) = open_trigger_root(candidate_usr, candidate_usr_path)?;
    let Some(trigger_root) = trigger_root else {
        return Ok(Vec::new());
    };
    let evaluators = trigger_declaration::system_evaluators();
    config::declaration::load_rooted_declarations(
        &trigger_root_path,
        &trigger_root,
        &evaluators,
    )
    .map_err(|source| Error::RootedTriggerDeclarations {
        source: Box::new(source),
    })
    .map(|loaded| {
        loaded
            .into_iter()
            .map(|loaded| {
                let SystemTrigger(trigger) = loaded.value;
                trigger
            })
            .collect_vec()
    })
}

fn open_trigger_root(
    candidate_usr: &File,
    candidate_usr_path: &Path,
) -> Result<(std::path::PathBuf, Option<File>), Error> {
    let trigger_root_path = candidate_usr_path.join(TRIGGER_RELATIVE_TO_USR);
    let trigger_root = match crate::linux_fs::openat2_file(
        candidate_usr.as_raw_fd(),
        c"share/cast/triggers",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        crate::linux_fs::controlled_resolution(),
    ) {
        Ok(root) => Some(root),
        Err(source) if source.kind() == io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(Error::OpenRetainedTriggerRoot {
                path: trigger_root_path.clone(),
                source,
            });
        }
    };
    Ok((trigger_root_path, trigger_root))
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

    #[test]
    fn system_codec_loads_a_lua_declaration_through_the_registered_extension() {
        let temporary = tempfile::tempdir().unwrap();
        let candidate_usr_path = temporary.path().join("staging").join("usr");
        let fragment = candidate_usr_path.join("share/cast/triggers/sys.d/system.lua");
        fs_err::create_dir_all(fragment.parent().unwrap()).unwrap();
        fs_err::write(&fragment, lua_trigger_source("lua-system", "/bin/true", "--quiet")).unwrap();
        let candidate_usr = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
            .open(&candidate_usr_path)
            .unwrap();

        let loaded = load_system(&candidate_usr, &candidate_usr_path).unwrap();

        assert_eq!(
            loaded.iter().map(|trigger| trigger.name.as_str()).collect_vec(),
            ["lua-system"]
        );
        assert!(matches!(
            loaded[0].handlers.get("lua-system"),
            Some(triggers::format::Handler::Run { run, args })
                if run == "/bin/true" && args == &["--quiet"]
        ));
    }

    fn lua_trigger_source(name: &str, command: &str, arg: &str) -> String {
        format!(
            r#"
return {{
    name = "{name}",
    description = "Lua retained trigger discovery fixture",
    before = {{ kind = "none" }},
    after = {{ kind = "none" }},
    inhibitors = {{ kind = "none" }},
    paths = {{
        {{
            key = "/usr/share/{name}",
            value = {{ handlers = {{ "{name}" }}, kind = {{ kind = "none" }} }},
        }},
    }},
    handlers = {{
        {{
            key = "{name}",
            value = {{ kind = "run", command = "{command}", args = {{ "{arg}" }} }},
        }},
    }},
}}
"#
        )
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
