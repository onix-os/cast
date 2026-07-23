use std::{collections::BTreeSet, fs, path::Path};

use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use crate::{
    Installation,
    package::{self, Meta, Name},
    state::Selection,
    test_support::private_installation_tempdir,
};

use super::{MutableSystemCapabilities, open_mutable_system_capabilities};

#[test]
fn production_capabilities_keep_install_state_layout_and_root_coherent_across_two_roots() {
    let first_temporary = private_installation_tempdir();
    let second_temporary = private_installation_tempdir();
    let first_root = fs::canonicalize(first_temporary.path()).unwrap();
    let second_root = fs::canonicalize(second_temporary.path()).unwrap();
    assert_ne!(first_root, second_root);

    let first = open_mutable_system_capabilities(Installation::open(&first_root, None).unwrap()).unwrap();
    let second = open_mutable_system_capabilities(Installation::open(&second_root, None).unwrap()).unwrap();
    let first_sentinel = Sentinel::new("first-root");
    let second_sentinel = Sentinel::new("second-root");

    first_sentinel.seed(&first);
    second_sentinel.seed(&second);

    first_sentinel.assert_only_sentinel(&first, &first_root);
    second_sentinel.assert_only_sentinel(&second, &second_root);
}

struct Sentinel {
    package: package::Id,
    metadata: Meta,
    layout: StonePayloadLayoutRecord,
    summary: String,
}

impl Sentinel {
    fn new(label: &str) -> Self {
        let metadata = Meta {
            name: Name::from(label.to_owned()),
            version_identifier: "1.0".to_owned(),
            source_release: 1,
            build_release: 1,
            architecture: "x86_64".to_owned(),
            summary: format!("{label} metadata sentinel"),
            description: format!("metadata retained by {label}"),
            source_id: format!("{label}-source"),
            homepage: format!("https://example.invalid/{label}"),
            licenses: vec!["MPL-2.0".to_owned()],
            dependencies: BTreeSet::new(),
            providers: BTreeSet::new(),
            conflicts: BTreeSet::new(),
            uri: None,
            hash: None,
            download_size: None,
        };
        Self {
            package: package::Id::from(metadata.id()),
            metadata,
            layout: StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: 0o755,
                tag: 0,
                file: StonePayloadLayoutFile::Directory(format!("share/{label}").into()),
            },
            summary: format!("{label} state sentinel"),
        }
    }

    fn seed(&self, system: &MutableSystemCapabilities) {
        system
            .install_db()
            .add(self.package.clone(), self.metadata.clone())
            .unwrap();
        system
            .state_db()
            .add(&[Selection::explicit(self.package.clone())], Some(&self.summary), None)
            .unwrap();
        system.layout_db().add(&self.package, &self.layout).unwrap();
    }

    fn assert_only_sentinel(&self, system: &MutableSystemCapabilities, root: &Path) {
        assert_eq!(system.installation().root, root);
        assert_eq!(
            system.install_db().package_ids().unwrap(),
            BTreeSet::from([self.package.clone()])
        );
        assert_eq!(system.install_db().get(&self.package).unwrap(), self.metadata);

        let states = system.state_db().all().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].summary.as_deref(), Some(self.summary.as_str()));
        assert_eq!(states[0].selections, vec![Selection::explicit(self.package.clone())]);

        assert_eq!(
            system.layout_db().all().unwrap(),
            vec![(self.package.clone(), self.layout.clone())]
        );
    }
}
