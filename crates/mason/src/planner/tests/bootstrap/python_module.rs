const PYTHON_BOOTSTRAP_PACKAGE_ID: &str = "57c7b5a7bda8628ee1b5943b58e9a672354e3948fb08f23cf6b137dde01bcc10";
const PYTHON_BUILD_BOOTSTRAP_PACKAGE_ID: &str = "72ece186ca5952eb4e2ded78b4a8f62bf61a606515c37352d1f74c204848eaed";
const PYTHON_INSTALLER_BOOTSTRAP_PACKAGE_ID: &str = "4a39d1b53afdf3505d0a349b4627c2386e9228ce0cab7aecbec735ce4c603af9";
const PYTHON_SETUPTOOLS_BOOTSTRAP_PACKAGE_ID: &str = "61c66d8caa536f2dd26ffe0724a79b6a9209a88ad9e040762413510e7afa5b0e";
const PYTHON_PYTEST_BOOTSTRAP_PACKAGE_ID: &str = "68732b606f6873f26b80c120225188c1a49036ed0abac9f9efdc6062523b7a36";
const PYTHON_TYPING_EXTENSIONS_BOOTSTRAP_PACKAGE_ID: &str =
    "64c5765414a46d8519e7c32b827ab3ac44ee71c9230e18b189a4dcf556ec3507";
const PYTHON_WHEEL_BOOTSTRAP_PACKAGE_ID: &str = "e0c9c5ca56eebce15488d3032746e84fa44c7ab1815ff07a0f5a365c9ed43736";

fn assert_python_module_bootstrap_contract(closure: &BootstrapClosure, indexed: &BTreeMap<String, Meta>) {
    assert_eq!(closure.packages.sha256.len(), 179, "bootstrap package count drift");
    assert_eq!(
        closure.packages.total_download_bytes, 388_713_448,
        "bootstrap download byte total drift"
    );

    let fixture = closure
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "python-module")
        .expect("missing bootstrap fixture `python-module`");
    assert_eq!(
        fixture.package_ids.len(),
        76,
        "python-module: package closure size drift"
    );
    let download_bytes = fixture
        .package_ids
        .iter()
        .map(|id| {
            indexed[id]
                .download_size
                .expect("Python closure package has no declared size")
        })
        .sum::<u64>();
    assert_eq!(
        download_bytes, 214_660_406,
        "python-module: closure download bytes drifted"
    );

    for (request, expected_id, expected_name) in [
        ("binary(bash)", GETTEXT_BASH_PACKAGE_ID, "bash"),
        ("binary(python3)", PYTHON_BOOTSTRAP_PACKAGE_ID, "python"),
        ("python(build)", PYTHON_BUILD_BOOTSTRAP_PACKAGE_ID, "python-build"),
        (
            "python(installer)",
            PYTHON_INSTALLER_BOOTSTRAP_PACKAGE_ID,
            "python-installer",
        ),
        (
            "python(setuptools)",
            PYTHON_SETUPTOOLS_BOOTSTRAP_PACKAGE_ID,
            "python-setuptools",
        ),
        ("python(pytest)", PYTHON_PYTEST_BOOTSTRAP_PACKAGE_ID, "python-pytest"),
        (
            "python(typing-extensions)",
            PYTHON_TYPING_EXTENSIONS_BOOTSTRAP_PACKAGE_ID,
            "python-typing_extensions",
        ),
    ] {
        let providers = fixture
            .package_ids
            .iter()
            .filter(|id| {
                indexed[*id]
                    .providers
                    .iter()
                    .any(|provider| provider.to_name() == request)
            })
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            providers,
            [expected_id],
            "python-module: {request} must have one exact provider in its frozen closure"
        );
        assert_eq!(indexed[expected_id].name.as_str(), expected_name);
    }

    for (id, name, version, source_release, download_size, uri) in [
        (
            PYTHON_BOOTSTRAP_PACKAGE_ID,
            "python",
            "3.14.6",
            23,
            10_916,
            "../../../legacy/pool/p/python/python-3.14.6-23-1-x86_64.stone",
        ),
        (
            PYTHON_BUILD_BOOTSTRAP_PACKAGE_ID,
            "python-build",
            "1.5.0",
            13,
            56_597,
            "../../../legacy/pool/p/python-build/python-build-1.5.0-13-1-x86_64.stone",
        ),
        (
            PYTHON_INSTALLER_BOOTSTRAP_PACKAGE_ID,
            "python-installer",
            "1.0.1",
            7,
            257_626,
            "../../../legacy/pool/p/python-installer/python-installer-1.0.1-7-1-x86_64.stone",
        ),
        (
            PYTHON_SETUPTOOLS_BOOTSTRAP_PACKAGE_ID,
            "python-setuptools",
            "82.0.1",
            13,
            1_216_136,
            "../../../legacy/pool/p/python-setuptools/python-setuptools-82.0.1-13-1-x86_64.stone",
        ),
        (
            PYTHON_PYTEST_BOOTSTRAP_PACKAGE_ID,
            "python-pytest",
            "9.1.1",
            8,
            841_802,
            "../../../pool/v0/p/python-pytest/python-pytest-9.1.1-8-1-x86_64.stone",
        ),
        (
            PYTHON_TYPING_EXTENSIONS_BOOTSTRAP_PACKAGE_ID,
            "python-typing_extensions",
            "4.15.0",
            6,
            94_716,
            "../../../legacy/pool/p/python-typing_extensions/python-typing_extensions-4.15.0-6-1-x86_64.stone",
        ),
        (
            PYTHON_WHEEL_BOOTSTRAP_PACKAGE_ID,
            "python-wheel",
            "0.47.0",
            10,
            71_821,
            "../../../legacy/pool/p/python-wheel/python-wheel-0.47.0-10-1-x86_64.stone",
        ),
    ] {
        assert!(fixture.package_ids.iter().any(|candidate| candidate == id));
        let package = &indexed[id];
        assert_eq!(package.name.as_str(), name);
        assert_eq!(package.version_identifier, version);
        assert_eq!(package.source_release, source_release);
        assert_eq!(package.build_release, 1);
        assert_eq!(package.architecture, "x86_64");
        assert_eq!(package.download_size, Some(download_size));
        assert_eq!(package.uri.as_deref(), Some(uri));
    }

    for (id, expected_providers, expected_dependencies) in [
        (
            PYTHON_BOOTSTRAP_PACKAGE_ID,
            &[
                "binary(pydoc)",
                "binary(pydoc3)",
                "binary(pydoc3.14)",
                "binary(python)",
                "binary(python3)",
                "binary(python3.14)",
                "python",
            ][..],
            &[
                "interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))",
                "soname(libc.so.6(x86_64))",
                "soname(libpython3.14.so.1.0(x86_64))",
            ][..],
        ),
        (
            PYTHON_BUILD_BOOTSTRAP_PACKAGE_ID,
            &["binary(pyproject-build)", "python-build", "python(build)"],
            &["binary(python)", "python(packaging)", "python(pyproject-hooks)"],
        ),
        (
            PYTHON_INSTALLER_BOOTSTRAP_PACKAGE_ID,
            &["python-installer", "python(installer)"],
            &[],
        ),
        (
            PYTHON_SETUPTOOLS_BOOTSTRAP_PACKAGE_ID,
            &["python-setuptools", "python(setuptools)"],
            &[
                "binary(python3)",
                "python(jaraco-functools)",
                "python(jaraco-text)",
                "python(more-itertools)",
                "python(packaging)",
                "python(wheel)",
            ],
        ),
        (
            PYTHON_PYTEST_BOOTSTRAP_PACKAGE_ID,
            &["binary(py.test)", "binary(pytest)", "python-pytest", "python(pytest)"],
            &[
                "binary(python3)",
                "python(iniconfig)",
                "python(packaging)",
                "python(pluggy)",
                "python(pygments)",
            ],
        ),
        (
            PYTHON_TYPING_EXTENSIONS_BOOTSTRAP_PACKAGE_ID,
            &["python-typing_extensions", "python(typing-extensions)"],
            &[],
        ),
        (
            PYTHON_WHEEL_BOOTSTRAP_PACKAGE_ID,
            &["binary(wheel)", "python-wheel", "python(wheel)"],
            &["binary(python)", "python(packaging)"],
        ),
    ] {
        let package = &indexed[id];
        assert_eq!(
            package
                .providers
                .iter()
                .map(|provider| provider.to_name())
                .collect::<BTreeSet<_>>(),
            expected_providers
                .iter()
                .map(|provider| (*provider).to_owned())
                .collect(),
            "python-module: {} provider metadata drifted",
            package.name
        );
        assert_eq!(
            package
                .dependencies
                .iter()
                .map(|dependency| dependency.to_name())
                .collect::<BTreeSet<_>>(),
            expected_dependencies
                .iter()
                .map(|dependency| (*dependency).to_owned())
                .collect(),
            "python-module: {} dependency metadata drifted",
            package.name
        );
    }

    let python_only = [
        PYTHON_BUILD_BOOTSTRAP_PACKAGE_ID,
        PYTHON_INSTALLER_BOOTSTRAP_PACKAGE_ID,
        PYTHON_SETUPTOOLS_BOOTSTRAP_PACKAGE_ID,
        PYTHON_PYTEST_BOOTSTRAP_PACKAGE_ID,
        PYTHON_TYPING_EXTENSIONS_BOOTSTRAP_PACKAGE_ID,
        PYTHON_WHEEL_BOOTSTRAP_PACKAGE_ID,
    ];
    for sibling in closure
        .fixtures
        .iter()
        .filter(|candidate| candidate.name != fixture.name)
    {
        for package_id in python_only {
            assert!(
                !sibling.package_ids.iter().any(|id| id == package_id),
                "{}: Python-module-only package {package_id} leaked into an unrelated closure",
                sibling.name
            );
        }
    }
}
