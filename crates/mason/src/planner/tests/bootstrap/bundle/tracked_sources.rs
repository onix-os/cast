fn fixture_source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gluon/execution/source-trees")
}

fn tracked_bytes(tree: &str, relative: &str) -> Vec<u8> {
    let root_path = fixture_source_root();
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&root_path)
        .unwrap_or_else(|error| panic!("open tracked fixture root {root_path:?} without following links: {error}"));
    assert!(
        root.metadata()
            .unwrap_or_else(|error| panic!("inspect tracked fixture root {root_path:?}: {error}"))
            .file_type()
            .is_dir(),
        "tracked fixture root {root_path:?} is not a directory"
    );

    let relative_path = Path::new(tree).join(relative);
    let components = relative_path
        .components()
        .map(|component| match component {
            Component::Normal(component) => component,
            _ => panic!("tracked source fixture path {relative_path:?} is not normalized and relative"),
        })
        .collect::<Vec<_>>();
    assert!(!components.is_empty(), "tracked source fixture path is empty");

    let mut parent = root;
    for component in &components[..components.len() - 1] {
        let component = CString::new(component.as_bytes())
            .unwrap_or_else(|_| panic!("tracked fixture directory component contains NUL"));
        // SAFETY: the parent is a live authenticated directory descriptor and
        // the component is normalized. O_NOFOLLOW rejects substituted links.
        let descriptor = unsafe {
            nix::libc::openat(
                parent.as_raw_fd(),
                component.as_ptr(),
                nix::libc::O_RDONLY
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK,
            )
        };
        assert!(
            descriptor >= 0,
            "open tracked fixture directory {relative_path:?} without following links: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: openat returned a fresh owned descriptor.
        parent = unsafe { File::from_raw_fd(descriptor) };
        let metadata = parent
            .metadata()
            .unwrap_or_else(|error| panic!("inspect tracked fixture directory {relative_path:?}: {error}"));
        assert!(
            metadata.file_type().is_dir(),
            "tracked fixture ancestor is not a directory"
        );
    }

    let component = CString::new(components.last().unwrap().as_bytes())
        .unwrap_or_else(|_| panic!("tracked fixture file component contains NUL"));
    // SAFETY: the parent descriptor is live and the final component is
    // normalized. O_NOFOLLOW prevents a final-component symlink substitution.
    let descriptor = unsafe {
        nix::libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            nix::libc::O_RDONLY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK
                | nix::libc::O_NOCTTY,
        )
    };
    assert!(
        descriptor >= 0,
        "open tracked source fixture {relative_path:?} without following links: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: openat returned a fresh owned descriptor.
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    let before = file
        .metadata()
        .unwrap_or_else(|error| panic!("inspect opened tracked source fixture {relative_path:?}: {error}"));
    assert!(
        before.file_type().is_file(),
        "tracked source fixture {relative_path:?} is not a regular file"
    );
    assert_eq!(
        before.nlink(),
        1,
        "tracked source fixture {relative_path:?} must have exactly one hard link"
    );
    assert_eq!(
        before.mode() & nix::libc::S_IFMT,
        nix::libc::S_IFREG,
        "tracked source fixture {relative_path:?} mode/type mismatch"
    );
    assert_eq!(
        before.mode() & 0o7000,
        0,
        "tracked source fixture {relative_path:?} must not carry special mode bits"
    );
    assert_eq!(
        before.mode() & 0o113,
        0,
        "tracked source fixture {relative_path:?} must be non-executable and non-world-writable"
    );
    assert!(
        before.size() <= MAX_TRACKED_FIXTURE_BYTES,
        "tracked source fixture {relative_path:?} exceeds its boundary"
    );
    let stamp = FileStamp::from_metadata(&before);
    let bytes = read_bounded(
        "tracked",
        &format!("source fixture {relative_path:?}"),
        &mut file,
        MAX_TRACKED_FIXTURE_BYTES,
    );
    assert_eq!(u64::try_from(bytes.len()).unwrap(), before.size());
    let after = file
        .metadata()
        .unwrap_or_else(|error| panic!("reinspect tracked source fixture {relative_path:?}: {error}"));
    assert_eq!(
        FileStamp::from_metadata(&after),
        stamp,
        "tracked source fixture {relative_path:?} changed while reading"
    );
    bytes
}

fn expected_split_pkgconfig() -> Vec<u8> {
    let template = String::from_utf8(tracked_bytes("cast-split-fixture-1.0.0", "cast-split.pc.in"))
        .expect("tracked pkg-config template must remain UTF-8");
    template
        .replace("@CMAKE_INSTALL_PREFIX@", "/usr")
        .replace("@CMAKE_INSTALL_LIBDIR@", "lib")
        .replace("@CMAKE_INSTALL_INCLUDEDIR@", "include")
        .replace("@PROJECT_VERSION@", "1.0.0")
        .into_bytes()
}
