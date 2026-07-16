
use std::{
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

use config::{LoadGluonError, SaveGluonError};
use fs_err as fs;

use super::*;

fn assert_portable_complete_fragment(fragment: &ProfileFragmentProvenance, host_root: &Path) {
    fragment.evaluation.validate().unwrap();
    assert!(!Path::new(&fragment.logical_name).is_absolute());
    assert!(!Path::new(&fragment.evaluation.root_logical_name).is_absolute());
    assert!(
        fragment
            .evaluation
            .imported_modules
            .iter()
            .all(|module| !Path::new(&module.logical_name).is_absolute())
    );

    let host_root = host_root.to_string_lossy();
    assert!(!fragment.logical_name.contains(host_root.as_ref()));
    assert!(!fragment.evaluation.root_logical_name.contains(host_root.as_ref()));
    assert!(
        fragment
            .evaluation
            .imported_modules
            .iter()
            .all(|module| !module.logical_name.contains(host_root.as_ref()))
    );
}

fn write(path: &Path, source: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

fn authored(body: &str) -> String {
    format!("let cast = import! cast.profile.v1\n{body}")
}

fn environment(config_root: &Path) -> Env {
    fs::set_permissions(config_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    Env::new(
        Some(config_root.join("cache")),
        Some(config_root.to_owned()),
        Some(config_root.join("data")),
        Some(config_root.join("forge")),
    )
    .unwrap()
}

fn single_profile(body: &str) -> String {
    authored(&format!("cast.profiles [cast.profile \"test\" [{body}]]"))
}

fn conversion_error(source: String) -> (PathBuf, String) {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("profile.d/invalid.glu");
    write(&path, &source);
    let error = config::Manager::custom(temporary.path())
        .load_gluon(&Evaluator::default(), &ProfileCodec)
        .expect_err("profile should be invalid");
    let LoadGluonError::Conversion {
        path: error_path,
        source,
    } = error
    else {
        panic!("expected conversion error");
    };
    (error_path, source.to_string())
}

#[test]
fn manager_loads_direct_root_and_repository_defaults() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("profile.d/authored.glu");
    write(
        &path,
        &authored(
            r#"cast.profiles [
    cast.profile "test" [
        cast.repository.direct "local" "file:///var/cache/local.index",
        cast.repository.root "volatile" "https://packages.example.test" "stream/volatile",
    ],
]"#,
        ),
    );

    let env = environment(temporary.path());
    let manager = Manager::new(&env).unwrap();
    assert_eq!(
        manager
            .fragments
            .iter()
            .map(|fragment| fragment.logical_name.as_str())
            .collect::<Vec<_>>(),
        ["authored"]
    );
    let fragment = &manager.fragments[0];
    assert_portable_complete_fragment(fragment, temporary.path());
    assert!(
        fragment
            .evaluation
            .imported_modules
            .iter()
            .any(|module| module.logical_name == "cast.profile.v1")
    );
    let repositories = manager.repositories(&Id::new("test")).unwrap();

    let local = repositories.get(&repository::Id::new("local")).unwrap();
    assert_eq!(local.description, "");
    assert_eq!(u64::from(local.priority), 0);
    assert!(local.active);
    assert!(matches!(&local.source, repository::Source::DirectIndex(_)));

    let volatile = repositories.get(&repository::Id::new("volatile")).unwrap();
    let repository::Source::RootIndex(root) = &volatile.source else {
        panic!("expected root-index repository");
    };
    assert_eq!(root.channel.as_ref(), repository::DEFAULT_CHANNEL);
    assert_eq!(root.arch, repository::DEFAULT_ARCH);
    assert_eq!(root.version.to_string(), "stream/volatile");
}

#[test]
fn active_root_indexes_must_match_the_selected_build_architecture() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("profile.d/authored.glu");
    write(
        &path,
        &authored(
            r#"cast.profiles [
    cast.profile "test" [
        cast.repository.direct "local" "file:///var/cache/local.index",
        cast.repository.root_index_with {
            id = "volatile",
            description = cast.optional.some "volatile",
            base_uri = "https://packages.example.test",
            channel = cast.optional.some "main",
            version = "stream/volatile",
            arch = cast.optional.some "x86_64",
            priority = cast.optional.some 0,
            enabled = cast.optional.some cast.boolean.true,
        },
        cast.repository.root_index_with {
            id = "disabled-aarch64",
            description = cast.optional.some "disabled",
            base_uri = "https://packages.example.test",
            channel = cast.optional.some "main",
            version = "stream/volatile",
            arch = cast.optional.some "aarch64",
            priority = cast.optional.some 0,
            enabled = cast.optional.some cast.boolean.false,
        },
    ],
]"#,
        ),
    );

    let env = environment(temporary.path());
    let manager = Manager::new(&env).unwrap();
    let profile = Id::new("test");
    assert!(manager.repositories_for_architecture(&profile, "x86_64").is_ok());
    assert!(matches!(
        manager.repositories_for_architecture(&profile, "aarch64"),
        Err(Error::RepositoryArchitectureMismatch {
            profile: error_profile,
            repository: repository_id,
            configured,
            requested,
        }) if error_profile == profile
            && repository_id == repository::Id::new("volatile")
            && configured == "x86_64"
            && requested == "aarch64"
    ));
}

#[test]
fn invalid_url_version_and_priority_report_exact_fields() {
    let (path, error) = conversion_error(single_profile(r#"cast.repository.direct "broken" "not a url""#));
    assert!(path.ends_with("profile.d/invalid.glu"));
    assert!(error.contains("profiles[0].repositories[0].source.uri"));

    let (_, error) = conversion_error(single_profile(
        r#"cast.repository.root "broken" "https://packages.example.test" "volatile""#,
    ));
    assert!(error.contains("profiles[0].repositories[0].source.version"));

    let (_, error) = conversion_error(single_profile(
        r#"cast.repository.direct_with {
    id = "broken",
    description = cast.optional.none,
    uri = "file:///valid.index",
    priority = cast.optional.some (-1),
    enabled = cast.optional.none,
}"#,
    ));
    assert!(error.contains("profiles[0].repositories[0].priority"));
}

#[test]
fn malformed_fragment_is_returned_by_the_manager_with_its_path() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("profile.d/malformed.glu");
    write(&path, "let value = in value");
    let env = environment(temporary.path());

    let error = match Manager::new(&env) {
        Ok(_) => panic!("malformed profile should fail"),
        Err(error) => error,
    };
    let Error::LoadProfiles(error) = error else {
        panic!("expected visible evaluation error");
    };
    let LoadGluonError::Evaluation {
        path: error_path,
        source,
    } = *error
    else {
        panic!("expected visible evaluation error");
    };
    assert_eq!(error_path, path);
    assert_eq!(source.source_name.as_deref(), Some("profile.d/malformed.glu"));
    assert!(source.span.is_some());
}

#[test]
fn generated_save_is_deterministic_standalone_and_loadable() {
    let evaluator = Evaluator::default();
    let decoded = ProfileCodec
        .decode(
            &evaluator,
            &GluonSource::new(
                "authored.glu",
                authored(
                    r#"cast.profiles [
    cast.profile "z-profile" [
        cast.repository.root "z-root" "https://packages.example.test" "stream/volatile",
        cast.repository.direct "a-direct" "file:///var/cache/local.index",
    ],
    cast.profile "a-profile" [],
]"#,
                ),
            ),
        )
        .unwrap();
    let first = ProfileCodec.encode(&decoded.value).unwrap();
    let repeated = ProfileCodec.encode(&decoded.value).unwrap();
    assert_eq!(first, repeated);
    assert!(first.find("id = \"a-profile\"").unwrap() < first.find("id = \"z-profile\"").unwrap());
    assert!(first.find("id = \"a-direct\"").unwrap() < first.find("id = \"z-root\"").unwrap());

    let temporary = tempfile::tempdir().unwrap();
    let manager = config::Manager::custom(temporary.path());
    let path = manager.save_gluon("generated", &decoded.value, &ProfileCodec).unwrap();
    let generated = fs::read_to_string(path).unwrap();
    assert!(generated.starts_with(config::GENERATED_GLUON_MARKER));
    assert!(generated.contains("type ProfileSpec ="));
    assert!(!generated.contains("import!"));

    let loaded = manager.load_gluon(&evaluator, &ProfileCodec).unwrap();
    assert_eq!(loaded.len(), 1);
    assert!(loaded[0].value.get(&Id::new("a-profile")).is_some());
    assert!(loaded[0].value.get(&Id::new("z-profile")).is_some());
}

#[test]
fn generated_save_refuses_to_overwrite_an_authored_fragment() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("profile.d/owned.glu");
    let source = authored("cast.profiles [cast.profile \"owned\" []]");
    write(&path, &source);
    let manager = config::Manager::custom(temporary.path());
    let loaded = manager.load_gluon(&Evaluator::default(), &ProfileCodec).unwrap();

    let error = manager
        .save_gluon("owned", &loaded[0].value, &ProfileCodec)
        .expect_err("authored fragment must be protected");
    assert!(matches!(error, SaveGluonError::AuthoredFragment { path: ref error_path } if error_path == &path));
    assert_eq!(fs::read_to_string(path).unwrap(), source);
}

#[test]
fn fragment_merge_order_is_deterministic() {
    let temporary = tempfile::tempdir().unwrap();
    write(
        &temporary.path().join("profile.d/z.glu"),
        &single_profile(r#"cast.repository.direct "source" "file:///z.index""#),
    );
    write(
        &temporary.path().join("profile.d/a.glu"),
        &single_profile(r#"cast.repository.direct "source" "file:///a.index""#),
    );
    let env = environment(temporary.path());
    let mut expected_fragments = None;

    for _ in 0..3 {
        let manager = Manager::new(&env).unwrap();
        assert_eq!(
            manager
                .fragments
                .iter()
                .map(|fragment| fragment.logical_name.as_str())
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
        for fragment in &manager.fragments {
            assert_portable_complete_fragment(fragment, temporary.path());
        }
        if let Some(expected) = &expected_fragments {
            assert_eq!(&manager.fragments, expected);
        } else {
            expected_fragments = Some(manager.fragments.clone());
        }
        let source = manager
            .repositories(&Id::new("test"))
            .unwrap()
            .get(&repository::Id::new("source"))
            .unwrap();
        let repository::Source::DirectIndex(uri) = &source.source else {
            panic!("expected direct-index repository");
        };
        assert_eq!(uri.as_str(), "file:///z.index");
    }
}

#[test]
fn saving_a_profile_refreshes_values_and_provenance_together() {
    let temporary = tempfile::tempdir().unwrap();
    let env = environment(temporary.path());
    let mut manager = Manager::new(&env).unwrap();

    manager
        .save_profile(
            Id::new("saved"),
            Profile {
                repositories: repository::Map::default(),
            },
        )
        .unwrap();

    assert!(manager.profiles.get(&Id::new("saved")).is_some());
    assert_eq!(
        manager
            .fragments
            .iter()
            .map(|fragment| fragment.logical_name.as_str())
            .collect::<Vec<_>>(),
        ["saved"]
    );
    assert_portable_complete_fragment(&manager.fragments[0], temporary.path());
}

#[test]
fn repository_owned_default_profile_is_valid_gluon() {
    let decoded = ProfileCodec
        .decode(
            &Evaluator::default(),
            &GluonSource::new(
                "default-x86_64.glu",
                include_str!("../../data/profile.d/default-x86_64.glu"),
            ),
        )
        .unwrap();
    let profile = decoded.value.get(&Id::new("default-x86_64")).unwrap();
    let volatile = profile.repositories.get(&repository::Id::new("volatile")).unwrap();
    assert_eq!(volatile.description, "AerynOS volatile stream (CDN)");
    assert!(volatile.active);
}
