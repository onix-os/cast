use std::{
    ffi::CString,
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{OpenOptionsExt, symlink},
    },
    sync::mpsc,
    thread,
    time::Duration,
};

use declarative_config::{DiagnosticCategory, LimitKind, SourceRoot};

#[test]
fn accepts_n_bytes_and_rejects_n_plus_one() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("bounded.glu");
    fs::write(&source_path, b"1234").unwrap();
    let root = SourceRoot::new(directory.path()).unwrap();

    let accepted = root.load("bounded.glu", 4).unwrap();
    assert_eq!(accepted.logical_name(), "bounded.glu");
    assert_eq!(accepted.text(), "1234");

    fs::write(source_path, b"12345").unwrap();
    let rejected = root.load("bounded.glu", 4).unwrap_err();
    assert_eq!(rejected.category, DiagnosticCategory::Limit);
    assert_eq!(rejected.limit, Some(LimitKind::SourceSize));
    assert_eq!(rejected.source_name.as_deref(), Some("bounded.glu"));
}

#[test]
fn rejects_a_symlink_file() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("real.glu"), "42").unwrap();
    symlink("real.glu", directory.path().join("linked.glu")).unwrap();
    let root = SourceRoot::new(directory.path()).unwrap();

    let error = root.load("linked.glu", 16).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Io);
    assert_eq!(error.source_name.as_deref(), Some("linked.glu"));
    assert!(error.message.contains("cannot contain symbolic links"));
}

#[test]
fn rejects_a_symlink_directory_component() {
    let directory = tempfile::tempdir().unwrap();
    let real = directory.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("value.glu"), "42").unwrap();
    symlink("real", directory.path().join("linked")).unwrap();
    let root = SourceRoot::new(directory.path()).unwrap();

    let error = root.load("linked/value.glu", 16).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Io);
    assert_eq!(error.source_name.as_deref(), Some("linked/value.glu"));
    assert!(error.message.contains("cannot contain symbolic links"));
}

#[test]
fn rejects_a_fifo_without_waiting_for_a_writer() {
    let directory = tempfile::tempdir().unwrap();
    let fifo = directory.path().join("blocking.glu");
    let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo_name is a valid NUL-terminated path and mode contains only
    // ordinary permission bits.
    let result = unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) };
    assert_eq!(result, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
    let root = SourceRoot::new(directory.path()).unwrap();
    let worker_root = root.clone();
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = sender.send(worker_root.load("blocking.glu", 16));
    });

    let loaded = match receiver.recv_timeout(Duration::from_secs(1)) {
        Ok(loaded) => loaded,
        Err(timeout) => {
            // Rescue a regressed blocking open so the test can join and fail
            // instead of leaving a stuck process behind.
            let _writer = fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo)
                .unwrap();
            let _ = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
            worker.join().unwrap();
            panic!("SourceRoot blocked while opening a FIFO: {timeout}");
        }
    };
    worker.join().unwrap();

    let error = loaded.unwrap_err();
    assert_eq!(error.category, DiagnosticCategory::Io);
    assert_eq!(error.source_name.as_deref(), Some("blocking.glu"));
    assert!(error.message.contains("not a regular file"));
}

#[test]
fn a_replaced_root_path_cannot_redirect_existing_loads() {
    let directory = tempfile::tempdir().unwrap();
    let configured_path = directory.path().join("root");
    let held_path = directory.path().join("held-root");
    fs::create_dir(&configured_path).unwrap();
    fs::write(configured_path.join("value.glu"), "old-root").unwrap();
    let canonical_path = configured_path.canonicalize().unwrap();
    let root = SourceRoot::new(&configured_path).unwrap();

    fs::rename(&configured_path, &held_path).unwrap();
    fs::create_dir(&configured_path).unwrap();
    fs::write(configured_path.join("value.glu"), "replacement-root").unwrap();
    let replacement_root = SourceRoot::new(&configured_path).unwrap();

    assert_eq!(root.path(), canonical_path);
    assert_eq!(replacement_root.path(), canonical_path);
    assert_ne!(root, replacement_root);
    let source = root.load("value.glu", 32).unwrap();
    assert_eq!(source.logical_name(), "value.glu");
    assert_eq!(source.text(), "old-root");
}
