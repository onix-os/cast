fn assert_frozen_provenance(
    fixture: &str,
    planned: &super::super::Planned,
    package: &str,
    records: &[StonePayloadMetaRecord],
) {
    let actual = records
        .iter()
        .filter_map(|record| match (&record.tag, &record.primitive) {
            (StonePayloadMetaTag::SourceRef, StonePayloadMetaPrimitive::String(value)) => Some(value.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let recipe = format!("gluon-evaluation-sha256:{}", planned.plan.provenance.recipe.sha256);
    let derivation = format!("derivation-sha256:{}", planned.plan.derivation_id());
    assert_eq!(
        actual,
        [recipe.as_str(), derivation.as_str()],
        "{fixture}: {package} provenance is not the exact canonical recipe-then-derivation sequence"
    );
}

fn assert_package_meta(
    fixture: &str,
    planned: &super::super::Planned,
    output: &OutputPlan,
    records: &[StonePayloadMetaRecord],
    meta: &Meta,
) {
    assert_eq!(meta.name.as_str(), output.package_name);
    assert_eq!(meta.version_identifier, planned.plan.package.version);
    assert_eq!(meta.source_release, planned.plan.package.source_release);
    assert_eq!(meta.build_release, planned.plan.package.build_release);
    assert_eq!(meta.architecture, planned.plan.package.architecture);
    assert_eq!(meta.summary, output.summary.as_deref().unwrap_or_default());
    assert_eq!(meta.description, output.description.as_deref().unwrap_or_default());
    assert_eq!(meta.source_id, planned.plan.package.name);
    assert_eq!(meta.homepage, planned.plan.package.homepage);
    let mut licenses = planned.plan.package.licenses.clone();
    licenses.sort();
    assert_eq!(meta.licenses, licenses);
    assert_eq!(meta.uri, None);
    assert_eq!(meta.hash, None);
    assert_eq!(meta.download_size, None);
    assert_eq!(
        meta.conflicts,
        output.conflicts.iter().map(|relation| relation.to_provider()).collect(),
        "{fixture}: {} conflicts drifted from the frozen output",
        output.package_name
    );

    let raw_dependencies = raw_dependencies(records, StonePayloadMetaTag::Depends);
    assert_eq!(
        raw_dependencies,
        meta.dependencies.iter().map(Dependency::to_name).collect(),
        "{fixture}: {} dependency records are not canonical",
        output.package_name
    );
    let mut decoded_providers = meta.providers.iter().map(Provider::to_name).collect::<BTreeSet<_>>();
    decoded_providers.remove(&output.package_name);
    assert_eq!(
        raw_providers(records, StonePayloadMetaTag::Provides),
        decoded_providers,
        "{fixture}: {} provider records are not canonical",
        output.package_name
    );
    assert_eq!(
        raw_providers(records, StonePayloadMetaTag::Conflicts),
        meta.conflicts.iter().map(Provider::to_name).collect(),
        "{fixture}: {} conflict records are not canonical",
        output.package_name
    );

    for relation in &output.runtime_inputs {
        let expected = match relation {
            OutputRelation::Locked { relation, .. } => relation.canonical_name(),
            OutputRelation::Planned {
                output: dependency_output,
            } => planned
                .plan
                .outputs
                .iter()
                .find(|candidate| candidate.name == *dependency_output)
                .unwrap_or_else(|| panic!("{fixture}: missing planned output dependency {dependency_output}"))
                .package_name
                .clone(),
        };
        assert!(
            raw_dependencies.contains(&expected),
            "{fixture}: {} omits frozen runtime relation {expected}",
            output.package_name
        );
    }
}

fn raw_dependencies(records: &[StonePayloadMetaRecord], tag: StonePayloadMetaTag) -> BTreeSet<String> {
    records
        .iter()
        .filter_map(|record| {
            if record.tag != tag {
                return None;
            }
            let StonePayloadMetaPrimitive::Dependency(kind, name) = &record.primitive else {
                unreachable!("metadata primitive was checked before relation extraction")
            };
            let kind = Kind::from_stone_dependency(*kind).expect("relation kind was checked");
            Some(Dependency::new(kind, name.clone()).unwrap().to_name())
        })
        .collect()
}

fn raw_providers(records: &[StonePayloadMetaRecord], tag: StonePayloadMetaTag) -> BTreeSet<String> {
    records
        .iter()
        .filter_map(|record| {
            if record.tag != tag {
                return None;
            }
            let StonePayloadMetaPrimitive::Provider(kind, name) = &record.primitive else {
                unreachable!("metadata primitive was checked before relation extraction")
            };
            let kind = Kind::from_stone_dependency(*kind).expect("relation kind was checked");
            Some(Provider::new(kind, name.clone()).unwrap().to_name())
        })
        .collect()
}

fn validate_layout_record(fixture: &str, package: &str, layout: &StonePayloadLayoutRecord) {
    assert_eq!(layout.uid, 0, "{fixture}: {package} layout owner must be root");
    assert_eq!(layout.gid, 0, "{fixture}: {package} layout group must be root");
    assert_eq!(layout.tag, 0, "{fixture}: {package} uses an unsupported layout tag");
    let target = layout.file.target();
    validate_target_path(fixture, package, target);

    let (file_type, expected_permissions) = match &layout.file {
        StonePayloadLayoutFile::Regular(_, _) => (nix::libc::S_IFREG, [0o644, 0o755].as_slice()),
        StonePayloadLayoutFile::Symlink(source, _) => {
            validate_symlink_source(fixture, package, target, source);
            (nix::libc::S_IFLNK, [0o777].as_slice())
        }
        StonePayloadLayoutFile::Directory(_) => (nix::libc::S_IFDIR, [0o755].as_slice()),
        StonePayloadLayoutFile::CharacterDevice(_)
        | StonePayloadLayoutFile::BlockDevice(_)
        | StonePayloadLayoutFile::Fifo(_)
        | StonePayloadLayoutFile::Socket(_)
        | StonePayloadLayoutFile::Unknown(_, _) => {
            panic!("{fixture}: {package} emits unsupported special layout /usr/{target}")
        }
    };
    assert_eq!(
        layout.mode & nix::libc::S_IFMT,
        file_type,
        "{fixture}: {package} layout type and mode disagree for /usr/{target}"
    );
    assert_eq!(
        layout.mode & !(nix::libc::S_IFMT | 0o7777),
        0,
        "{fixture}: {package} layout has unsupported mode bits for /usr/{target}"
    );
    assert_eq!(
        layout.mode & 0o7000,
        0,
        "{fixture}: {package} layout must not carry setuid, setgid, or sticky bits"
    );
    assert!(
        expected_permissions.contains(&(layout.mode & 0o777)),
        "{fixture}: {package} layout has unexpected permissions {:o} for /usr/{target}",
        layout.mode & 0o777
    );
}

fn validate_target_path(fixture: &str, package: &str, target: &str) {
    assert!(!target.is_empty(), "{fixture}: {package} has an empty layout target");
    assert!(target.len() <= 4_096, "{fixture}: {package} layout target is too long");
    assert!(
        !target.starts_with('/'),
        "{fixture}: {package} layout target must be /usr-relative"
    );
    assert!(
        !target.ends_with('/'),
        "{fixture}: {package} layout target has a trailing separator"
    );
    assert!(
        !target.bytes().any(|byte| byte == 0 || byte.is_ascii_control()),
        "{fixture}: {package} layout target contains a control byte"
    );
    let components = Path::new(target)
        .components()
        .map(|component| match component {
            Component::Normal(component) => component
                .to_str()
                .unwrap_or_else(|| panic!("{fixture}: {package} layout target is not UTF-8")),
            _ => panic!("{fixture}: {package} layout target is not a normalized relative path: {target:?}"),
        })
        .collect::<Vec<_>>();
    assert!(!components.is_empty());
    assert!(components.len() <= 64, "{fixture}: {package} layout target is too deep");
    assert_eq!(
        components.join("/"),
        target,
        "{fixture}: {package} layout target is not canonical"
    );
}

fn validate_symlink_source(fixture: &str, package: &str, target: &str, source: &str) {
    assert!(
        !source.is_empty(),
        "{fixture}: {package} symlink /usr/{target} has an empty source"
    );
    assert!(source.len() <= 4_096, "{fixture}: {package} symlink source is too long");
    assert!(
        !source.bytes().any(|byte| byte == 0 || byte.is_ascii_control()),
        "{fixture}: {package} symlink source contains a control byte"
    );
    assert!(
        !Path::new(source).is_absolute(),
        "{fixture}: {package} fixture symlink /usr/{target} must use a relative source"
    );
    let _ = resolve_symlink_target(fixture, package, target, source);
}

fn resolve_symlink_target(fixture: &str, package: &str, target: &str, source: &str) -> String {
    let mut resolved = Path::new(target)
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for component in Path::new(source).components() {
        match component {
            Component::Normal(value) => resolved.push(value.to_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                assert!(
                    resolved.pop().is_some(),
                    "{fixture}: {package} symlink /usr/{target} escapes /usr"
                );
            }
            Component::RootDir | Component::Prefix(_) => {
                panic!("{fixture}: {package} symlink /usr/{target} is not relative")
            }
        }
    }
    let resolved = resolved
        .iter()
        .map(|component| component.to_str().expect("validated UTF-8 symlink component"))
        .collect::<Vec<_>>()
        .join("/");
    validate_target_path(fixture, package, &resolved);
    resolved
}

fn assert_global_layout_integrity(fixture: &str, packages: &BTreeMap<String, PackageImage>) {
    let mut global = BTreeMap::<String, (&str, &StonePayloadLayoutRecord)>::new();
    for (package, image) in packages {
        for (target, layout) in &image.layouts {
            assert!(
                global.insert(target.clone(), (package, layout)).is_none(),
                "{fixture}: /usr/{target} is emitted by more than one output"
            );
        }
    }

    for (target, (package, layout)) in &global {
        let mut ancestor = Path::new(target).parent();
        while let Some(path) = ancestor {
            if path.as_os_str().is_empty() {
                break;
            }
            let path = path.to_str().expect("validated UTF-8 layout path");
            if let Some((ancestor_package, ancestor_layout)) = global.get(path) {
                assert!(
                    matches!(&ancestor_layout.file, StonePayloadLayoutFile::Directory(_)),
                    "{fixture}: terminal /usr/{path} from {ancestor_package} is an ancestor of /usr/{target} from {package}"
                );
            }
            ancestor = Path::new(path).parent();
        }

        if let StonePayloadLayoutFile::Symlink(source, _) = &layout.file {
            let mut resolved = resolve_symlink_target(fixture, package, target, source);
            let mut visited = BTreeSet::from([target.clone()]);
            loop {
                assert!(
                    visited.insert(resolved.clone()),
                    "{fixture}: package symlink cycle reaches /usr/{resolved}"
                );
                let (resolved_package, resolved_layout) = global.get(&resolved).unwrap_or_else(|| {
                    panic!(
                        "{fixture}: {package} symlink /usr/{target} resolves to missing package path /usr/{resolved}"
                    )
                });
                match &resolved_layout.file {
                    StonePayloadLayoutFile::Symlink(next, _) => {
                        resolved = resolve_symlink_target(fixture, resolved_package, &resolved, next);
                    }
                    StonePayloadLayoutFile::Regular(_, _) | StonePayloadLayoutFile::Directory(_) => break,
                    _ => panic!("{fixture}: symlink /usr/{target} resolves to an unsupported inode"),
                }
            }
        }
    }
}
