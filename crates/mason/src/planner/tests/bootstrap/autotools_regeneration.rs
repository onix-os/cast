const AUTOCONF_PACKAGE_ID: &str = "8103d16d5d75df5a1d57f2de5629cca69e38aece8b3f5d60a1ff47265bfd2cbf";
const AUTOMAKE_PACKAGE_ID: &str = "9a8d3961effd5bd65ed6a024f149cb6836acfbc5c5feab750a78e44cd4cf9356";
const INSTALL_PACKAGE_ID: &str = "1a3f33a18144f93019f9572be47ce56ec60b79707a8e0678df0acbc98699a9cf";

fn assert_autotools_regeneration_bootstrap_contract(
    closure: &BootstrapClosure,
    indexed: &BTreeMap<String, Meta>,
) {
    let fixture = |name: &str| {
        closure
            .fixtures
            .iter()
            .find(|fixture| fixture.name == name)
            .unwrap_or_else(|| panic!("missing bootstrap fixture `{name}`"))
    };
    let autotools = fixture("autotools");
    let options = fixture("autotools-options");
    assert_eq!(autotools.package_ids.len(), 66, "autotools: package closure size drift");
    assert_eq!(
        autotools.package_ids, options.package_ids,
        "autotools regeneration must not enlarge the existing exact tool closure"
    );
    for required in [AUTOCONF_PACKAGE_ID, AUTOMAKE_PACKAGE_ID, INSTALL_PACKAGE_ID] {
        assert!(
            autotools.package_ids.iter().any(|id| id == required),
            "autotools: regeneration closure is missing {required}"
        );
        assert!(
            closure.packages.sha256.iter().any(|id| id == required),
            "bootstrap aggregate is missing Autotools package {required}"
        );
    }
    for (request, expected_id, expected_name) in [
        ("binary(autoconf)", AUTOCONF_PACKAGE_ID, "autoconf"),
        ("binary(automake)", AUTOMAKE_PACKAGE_ID, "automake"),
        ("binary(autoreconf)", AUTOCONF_PACKAGE_ID, "autoconf"),
        ("binary(install)", INSTALL_PACKAGE_ID, "uutils-coreutils"),
    ] {
        let providers = autotools
            .package_ids
            .iter()
            .filter(|id| indexed[*id].providers.iter().any(|provider| provider.to_name() == request))
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            providers,
            [expected_id],
            "autotools: {request} must have one exact provider in its frozen closure"
        );
        assert_eq!(indexed[expected_id].name.as_str(), expected_name);
    }

    let autoconf = &indexed[AUTOCONF_PACKAGE_ID];
    assert_eq!(autoconf.name.as_str(), "autoconf");
    assert_eq!(autoconf.version_identifier, "2.73");
    assert_eq!(autoconf.source_release, 6);
    assert_eq!(autoconf.build_release, 1);
    assert_eq!(autoconf.download_size, Some(571_217));
    assert_eq!(
        autoconf.uri.as_deref(),
        Some("../../../legacy/pool/a/autoconf/autoconf-2.73-6-1-x86_64.stone")
    );
    assert_eq!(
        autoconf.providers.iter().map(|provider| provider.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "autoconf".to_owned(),
            "binary(autoconf)".to_owned(),
            "binary(autoheader)".to_owned(),
            "binary(autom4te)".to_owned(),
            "binary(autoreconf)".to_owned(),
            "binary(autoscan)".to_owned(),
            "binary(autoupdate)".to_owned(),
            "binary(ifnames)".to_owned(),
        ])
    );
    assert_eq!(
        autoconf.dependencies.iter().map(|dependency| dependency.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "binary(m4)".to_owned(),
            "binary(perl)".to_owned(),
            "binary(slibtool)".to_owned(),
        ])
    );

    let automake = &indexed[AUTOMAKE_PACKAGE_ID];
    assert_eq!(automake.name.as_str(), "automake");
    assert_eq!(automake.version_identifier, "1.18.1");
    assert_eq!(automake.source_release, 8);
    assert_eq!(automake.build_release, 1);
    assert_eq!(automake.download_size, Some(585_934));
    assert_eq!(
        automake.uri.as_deref(),
        Some("../../../legacy/pool/a/automake/automake-1.18.1-8-1-x86_64.stone")
    );
    assert_eq!(
        automake.providers.iter().map(|provider| provider.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "automake".to_owned(),
            "binary(aclocal)".to_owned(),
            "binary(aclocal-1.18)".to_owned(),
            "binary(automake)".to_owned(),
            "binary(automake-1.18)".to_owned(),
        ])
    );
    assert_eq!(
        automake.dependencies.iter().map(|dependency| dependency.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from(["binary(autoconf)".to_owned()])
    );
}

fn assert_autotools_regeneration_relations(plan: &stone_recipe::derivation::DerivationPlan) {
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        [
            "binary(autoconf)",
            "binary(automake)",
            "binary(install)",
            "binary(autoreconf)",
        ],
        "autotools: manifest BuildDepends inputs drifted"
    );

    let builder_origin = |index| stone_recipe::derivation::InputOrigin::BuilderTool {
        selection: stone_recipe::derivation::PackageInputSelection::Package,
        index,
    };
    let assert_request = |
        name: &str,
        package_id: &str,
        origins: Vec<stone_recipe::derivation::InputOrigin>,
    | {
        let requests = plan
            .build_lock
            .requests
            .iter()
            .filter(|request| request.request == name)
            .collect::<Vec<_>>();
        let [request] = requests.as_slice() else {
            panic!("autotools: build lock must contain exactly one {name} request");
        };
        assert_eq!(request.package_id, package_id, "autotools: {name} provider drifted");
        assert_eq!(request.output, "out", "autotools: {name} output drifted");
        assert_eq!(request.origins, origins, "autotools: {name} origins drifted");
    };
    assert_request("binary(autoconf)", AUTOCONF_PACKAGE_ID, vec![builder_origin(0)]);
    assert_request("binary(automake)", AUTOMAKE_PACKAGE_ID, vec![builder_origin(1)]);
    assert_request("binary(install)", INSTALL_PACKAGE_ID, vec![builder_origin(2)]);
    assert_request(
        "binary(autoreconf)",
        AUTOCONF_PACKAGE_ID,
        vec![
            stone_recipe::derivation::InputOrigin::NativeBuild {
                selection: stone_recipe::derivation::PackageInputSelection::Package,
                index: 0,
            },
            stone_recipe::derivation::InputOrigin::JobExecutable {
                job: 0,
                phase: 1,
                phase_name: "Setup".to_owned(),
                section: stone_recipe::derivation::JobStepSection::Pre,
                step: 0,
                role: stone_recipe::derivation::JobExecutableRole::RunProgram,
            },
        ],
    );

    let autoconf = plan
        .build_lock
        .packages
        .iter()
        .find(|package| package.package_id == AUTOCONF_PACKAGE_ID)
        .expect("autotools: autoconf provider package is absent");
    assert_eq!((autoconf.name.as_str(), autoconf.version.as_str()), ("autoconf", "2.73-6-1"));
    assert_eq!(autoconf.architecture, "x86_64");
    assert_eq!(autoconf.repository, "bootstrap");
    assert_eq!(autoconf.outputs.iter().map(|output| output.name.as_str()).collect::<Vec<_>>(), ["out"]);

    let [job] = plan.jobs.as_slice() else {
        panic!("autotools: regeneration fixture must freeze exactly one job");
    };
    let setup = job
        .phases
        .iter()
        .find(|phase| phase.name == "Setup")
        .expect("autotools: frozen Setup phase is missing");
    let [stone_recipe::derivation::StepPlan::Run {
        program,
        args,
        ..
    }] = setup.pre.as_slice()
    else {
        panic!("autotools: frozen Setup prelude must contain exactly one Run step");
    };
    assert_eq!(program.path, "/usr/bin/autoreconf");
    assert_eq!(program.requirement.canonical_name(), "binary(autoreconf)");
    assert_eq!(args.as_slice(), ["-fi"]);
    let [stone_recipe::derivation::StepPlan::Run {
        program,
        args,
        ..
    }] = setup.steps.as_slice()
    else {
        panic!("autotools: frozen Setup body must contain exactly one configure step");
    };
    assert_eq!(program.path, "/usr/bin/dash");
    assert_eq!(program.requirement.canonical_name(), "binary(dash)");
    assert_eq!(
        args.as_slice(),
        [
            "./configure",
            "--prefix=/usr",
            "--bindir=/usr/bin",
            "--sbindir=/usr/sbin",
            "--build=x86_64-aerynos-linux",
            "--host=x86_64-aerynos-linux",
            "--libdir=/usr/lib",
            "--mandir=/usr/share/man",
            "--infodir=/usr/share/info",
            "--datadir=/usr/share",
            "--sysconfdir=/etc",
            "--localstatedir=/var",
            "--sharedstatedir=/var/lib",
            "--libexecdir=/usr/lib/cast-autotools-fixture",
        ],
        "autotools: frozen configure argv must remain exact and cardinality-bound"
    );
}
