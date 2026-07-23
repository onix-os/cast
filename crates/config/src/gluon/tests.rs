use std::{
    cell::Cell,
    os::unix::fs::symlink,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use super::*;
use crate::Scope;
use gluon_config::{DiagnosticCategory, LimitKind, Limits};

impl Config for String {
    fn domain() -> String {
        "dummy".to_owned()
    }
}

struct StringCodec;

impl GluonCodec for StringCodec {
    type Config = String;

    fn decode(&self, evaluator: &Evaluator, source: &Source) -> Result<DecodedGluon<Self::Config>, GluonCodecError> {
        let evaluation = evaluator.evaluate::<String>(source)?;
        if evaluation.value == "reject" {
            return Err(GluonCodecError::conversion(io::Error::new(
                io::ErrorKind::InvalidData,
                "rejected dummy value",
            )));
        }
        Ok(evaluation.into())
    }

    fn encode(&self, config: &Self::Config) -> Result<String, GluonCodecError> {
        Ok(format!("\"{config}\""))
    }
}

struct RawCodec;

impl GluonCodec for RawCodec {
    type Config = String;

    fn decode(&self, evaluator: &Evaluator, source: &Source) -> Result<DecodedGluon<Self::Config>, GluonCodecError> {
        Ok(evaluator.evaluate::<String>(source)?.into())
    }

    fn encode(&self, config: &Self::Config) -> Result<String, GluonCodecError> {
        Ok(config.clone())
    }
}

struct DeadlineBoundaryCodec<'a> {
    enumeration_finished: &'a Cell<bool>,
    outside_delay: Duration,
}

impl GluonCodec for DeadlineBoundaryCodec<'_> {
    type Config = String;

    fn decode(&self, evaluator: &Evaluator, source: &Source) -> Result<DecodedGluon<Self::Config>, GluonCodecError> {
        assert!(self.enumeration_finished.get());
        assert_eq!(source.text(), "\"value\"");

        // Manager has already read the root source, but Evaluator has not yet
        // created its per-call deadline.
        thread::sleep(self.outside_delay);
        let evaluation = evaluator.evaluate::<String>(source)?;

        // Typed/domain conversion happens after Evaluator has stopped
        // enforcing that deadline.
        thread::sleep(self.outside_delay);
        Ok(DecodedGluon {
            value: format!("converted:{}", evaluation.value),
            fingerprint: evaluation.fingerprint,
        })
    }

    fn encode(&self, config: &Self::Config) -> Result<String, GluonCodecError> {
        Ok(format!("\"{config}\""))
    }
}

fn write(path: impl AsRef<Path>, content: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn layered_manager(root: &Path, user: &Path, program: &str) -> Manager {
    Manager {
        scope: Scope::User {
            root: root.to_owned(),
            config: user.to_owned(),
            program: program.to_owned(),
        },
    }
}

fn mkfifo(path: &Path) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: path is a valid NUL-terminated pathname and mode contains
    // only ordinary permission bits.
    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(result, 0, "mkfifo failed: {}", io::Error::last_os_error());
}

fn finish_nonblocking<T: Send + 'static>(fifo: &Path, operation: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = sender.send(operation());
    });
    let result = match receiver.recv_timeout(Duration::from_secs(1)) {
        Ok(result) => result,
        Err(timeout) => {
            // Rescue a regressed blocking FIFO open so this test fails
            // instead of leaving the test process stuck forever.
            let writer = fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(fifo)
                .unwrap();
            drop(writer);
            let _ = receiver.recv_timeout(Duration::from_secs(1));
            worker.join().unwrap();
            panic!("managed fragment operation blocked on a FIFO: {timeout}");
        }
    };
    worker.join().unwrap();
    result
}

fn generated_literal_with_total_size(total: usize) -> String {
    let literal_len = total.checked_sub(GENERATED_GLUON_MARKER.len()).unwrap();
    assert!(literal_len > 0);
    let mut literal = "x".repeat(literal_len);
    literal.replace_range(literal_len - 1.., "\n");
    literal
}

fn temporary_fragment_names(directory: &Path) -> Vec<OsString> {
    let mut names = fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name.as_bytes().starts_with(TEMPORARY_PREFIX.as_bytes()))
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn assert_no_temporary_fragments(directory: &Path) {
    assert_eq!(temporary_fragment_names(directory), Vec::<OsString>::new());
}

#[test]
fn gluon_fragments_have_deterministic_order_and_explicit_precedence() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let user = temporary.path().join("user");
    let program = "config-gluon-precedence";
    let vendor = root.join("usr/share").join(program);
    let admin = root.join("etc").join(program);
    let user_program = user.join(program);

    write(vendor.join("dummy.glu"), "\"vendor-base\"");
    write(vendor.join("dummy.d/z.glu"), "\"vendor-z\"");
    write(vendor.join("dummy.d/a.glu"), "\"vendor-a\"");
    write(vendor.join("dummy.d/shared.glu"), "\"vendor-shared\"");
    write(admin.join("dummy.glu"), "\"admin-base\"");
    write(admin.join("dummy.d/shared.glu"), "\"admin-shared\"");
    write(user_program.join("dummy.glu"), "\"user-base\"");
    write(user_program.join("dummy.d/shared.glu"), "\"user-shared\"");

    let loaded = layered_manager(&root, &user, program)
        .load_gluon(&Evaluator::default(), &StringCodec)
        .unwrap();
    assert_eq!(
        loaded
            .iter()
            .map(|fragment| (fragment.logical_name.as_str(), fragment.value.as_str()))
            .collect::<Vec<_>>(),
        [
            ("a", "vendor-a"),
            ("dummy", "user-base"),
            ("shared", "user-shared"),
            ("z", "vendor-z"),
        ]
    );
}

#[test]
fn shadowed_lower_priority_fragment_is_still_evaluated_and_can_fail() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let user = temporary.path().join("user");
    let program = "config-gluon-shadow-validation";
    let lower = root
        .join("usr/share")
        .join(program)
        .join("dummy.d/shared.glu");
    let higher = user.join(program).join("dummy.d/shared.glu");
    write(&lower, "let value = in value");
    write(&higher, "\"valid higher layer\"");

    let error = layered_manager(&root, &user, program)
        .load_gluon(&Evaluator::default(), &StringCodec)
        .unwrap_err();
    let LoadGluonError::Evaluation { path, source } = error else {
        panic!("invalid lower-priority fragment did not reach evaluation");
    };
    assert_eq!(path, lower);
    assert_eq!(source.category, DiagnosticCategory::Parse);
    assert_eq!(source.source_name.as_deref(), Some("dummy.d/shared.glu"));
}

#[test]
fn manager_work_surrounding_evaluation_is_outside_the_fragment_deadline() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    write(temporary.path().join("dummy.d/value.glu"), "\"value\"");

    let evaluator_timeout = Duration::from_millis(200);
    let outside_delay = Duration::from_millis(250);
    let evaluator = Evaluator::new(Limits {
        timeout: evaluator_timeout,
        ..Limits::default()
    });
    // Keep this characterization about deadline placement rather than the
    // first Gluon VM initialization on an unusually slow test worker.
    assert_eq!(
        evaluator
            .evaluate::<String>(&Source::new("warmup.glu", "\"warm\""))
            .unwrap()
            .value,
        "warm"
    );

    let enumeration_finished = Cell::new(false);
    let codec = DeadlineBoundaryCodec {
        enumeration_finished: &enumeration_finished,
        outside_delay,
    };
    let started = Instant::now();
    let loaded = manager
        .load_gluon_with_hook(&evaluator, &codec, || {
            // collect_gluon_paths has completed, but no per-fragment evaluator
            // deadline exists yet.
            enumeration_finished.set(true);
            thread::sleep(outside_delay);
        })
        .unwrap();

    assert!(enumeration_finished.get());
    assert_eq!(loaded[0].value, "converted:value");
    assert!(started.elapsed() >= outside_delay * 3);
    assert!(started.elapsed() > evaluator_timeout);
}

#[test]
fn authored_fragment_reads_accept_n_bytes_and_reject_n_plus_one() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let fragment = temporary.path().join("dummy.d/bounded.glu");
    let mut limits = Limits::default();
    limits.max_source_bytes = 6;
    let evaluator = Evaluator::new(limits);

    write(&fragment, "\"1234\"");
    let loaded = manager.load_gluon(&evaluator, &StringCodec).unwrap();
    assert_eq!(loaded[0].value, "1234");

    write(&fragment, "\"12345\"");
    let error = manager.load_gluon(&evaluator, &StringCodec).unwrap_err();
    let LoadGluonError::Evaluation { path, source } = error else {
        panic!("expected source-size evaluation error");
    };
    assert_eq!(path, fragment);
    assert_eq!(source.category, DiagnosticCategory::Limit);
    assert_eq!(source.limit, Some(LimitKind::SourceSize));
}

#[test]
fn fragment_count_accepts_n_and_rejects_n_plus_one_before_evaluation() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("dummy.d");
    fs::create_dir(&directory).unwrap();
    let manager = Manager::custom(temporary.path());
    for index in 0..MAX_GLUON_FRAGMENTS {
        fs::write(directory.join(format!("fragment-{index:04}.glu")), "not evaluated").unwrap();
    }

    assert_eq!(
        collect_gluon_paths(&manager.scope, "dummy").unwrap().len(),
        MAX_GLUON_FRAGMENTS
    );

    fs::write(directory.join("overflow.glu"), "not evaluated").unwrap();
    let error = collect_gluon_paths(&manager.scope, "dummy").unwrap_err();
    assert!(matches!(
        error,
        LoadGluonError::FragmentLimit {
            limit: MAX_GLUON_FRAGMENTS
        }
    ));
}

#[test]
fn directory_scan_accepts_n_ignored_entries_and_rejects_n_plus_one() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("dummy.d");
    fs::create_dir(&directory).unwrap();
    let manager = Manager::custom(temporary.path());
    for index in 0..MAX_GLUON_DIRECTORY_ENTRIES {
        fs::write(directory.join(format!("ignored-{index:04}.txt")), "ignored").unwrap();
    }

    assert!(collect_gluon_paths(&manager.scope, "dummy").unwrap().is_empty());

    fs::write(directory.join("overflow.txt"), "ignored").unwrap();
    let error = collect_gluon_paths(&manager.scope, "dummy").unwrap_err();
    assert!(matches!(
        error,
        LoadGluonError::DirectoryEntryLimit {
            path,
            limit: MAX_GLUON_DIRECTORY_ENTRIES
        } if path == directory
    ));
}

#[test]
fn matching_symlinks_are_rejected_instead_of_followed() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let outside = temporary.path().join("outside.glu");
    let linked = temporary.path().join("dummy.d/linked.glu");
    write(&outside, "\"outside\"");
    fs::create_dir_all(linked.parent().unwrap()).unwrap();
    symlink(&outside, &linked).unwrap();

    let error = manager.load_gluon(&Evaluator::default(), &StringCodec).unwrap_err();

    assert!(matches!(error, LoadGluonError::Enumerate { path, .. } if path == linked));
}

#[test]
fn a_symlinked_fragment_collection_is_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let real = temporary.path().join("real-fragments");
    let linked = temporary.path().join("dummy.d");
    write(real.join("value.glu"), "\"outside\"");
    symlink(&real, &linked).unwrap();

    let error = manager.load_gluon(&Evaluator::default(), &StringCodec).unwrap_err();

    assert!(matches!(error, LoadGluonError::Enumerate { path, .. } if path == linked));
}

#[test]
fn an_open_collection_descriptor_cannot_be_redirected() {
    let temporary = tempfile::tempdir().unwrap();
    let collection = temporary.path().join("dummy.d");
    let held = temporary.path().join("held");
    let outside = temporary.path().join("outside");
    write(collection.join("inside.txt"), "inside");
    write(outside.join("outside.txt"), "outside");
    let directory = FragmentDirectory::open(&collection).unwrap();

    fs::rename(&collection, &held).unwrap();
    symlink(&outside, &collection).unwrap();
    let BoundedDirectoryEntries::Complete(names) = directory.entry_names(8).unwrap() else {
        panic!("one held entry cannot exceed the scan limit");
    };
    assert_eq!(names, [OsString::from("inside.txt")]);

    let BoundedDirectoryEntries::Complete(repeated) = directory.entry_names(8).unwrap() else {
        panic!("a repeated held-directory scan cannot exceed the limit");
    };
    assert_eq!(repeated, [OsString::from("inside.txt")]);
}

#[test]
fn matching_fifos_are_rejected_without_waiting_for_a_writer() {
    let temporary = tempfile::tempdir().unwrap();
    let fifo = temporary.path().join("dummy.d/blocking.glu");
    fs::create_dir_all(fifo.parent().unwrap()).unwrap();
    mkfifo(&fifo);
    let manager = Manager::custom(temporary.path());
    let fifo_for_timeout = fifo.clone();

    let result = finish_nonblocking(&fifo_for_timeout, move || {
        manager.load_gluon(&Evaluator::default(), &StringCodec)
    });

    assert!(matches!(result, Err(LoadGluonError::Enumerate { path, .. }) if path == fifo));
}

#[test]
fn replacing_a_layer_root_during_load_fails_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let configured = temporary.path().join("configured");
    let held = temporary.path().join("held");
    write(configured.join("dummy.d/value.glu"), "\"original\"");
    let manager = Manager::custom(&configured);

    let error = manager
        .load_gluon_with_hook(&Evaluator::default(), &StringCodec, || {
            fs::rename(&configured, &held).unwrap();
            write(configured.join("dummy.d/value.glu"), "\"replacement\"");
        })
        .unwrap_err();

    assert!(matches!(error, LoadGluonError::Enumerate { source, .. }
        if source.to_string().contains("source root changed")));
}

#[test]
fn replacing_an_enumerated_fragment_with_a_symlink_fails_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let fragment = temporary.path().join("dummy.d/value.glu");
    let outside = temporary.path().join("outside.glu");
    write(&fragment, "\"original\"");
    write(&outside, "\"replacement\"");
    let manager = Manager::custom(temporary.path());

    let error = manager
        .load_gluon_with_hook(&Evaluator::default(), &StringCodec, || {
            fs::remove_file(&fragment).unwrap();
            symlink(&outside, &fragment).unwrap();
        })
        .unwrap_err();

    assert!(matches!(error, LoadGluonError::Enumerate { path, source }
        if path == fragment.parent().unwrap()
            && source.to_string().contains("fragment collection changed")));
}

#[test]
fn replacing_an_enumerated_collection_fails_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let collection = temporary.path().join("dummy.d");
    let held = temporary.path().join("held");
    write(collection.join("value.glu"), "\"original\"");
    let manager = Manager::custom(temporary.path());

    let error = manager
        .load_gluon_with_hook(&Evaluator::default(), &StringCodec, || {
            fs::rename(&collection, &held).unwrap();
            write(collection.join("value.glu"), "\"replacement\"");
        })
        .unwrap_err();

    assert!(matches!(error, LoadGluonError::Enumerate { source, .. }
        if source.to_string().contains("fragment collection changed")));
}

#[test]
fn changing_an_enumerated_collection_fails_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let collection = temporary.path().join("dummy.d");
    write(collection.join("value.glu"), "\"original\"");
    let manager = Manager::custom(temporary.path());

    let error = manager
        .load_gluon_with_hook(&Evaluator::default(), &StringCodec, || {
            fs::write(collection.join("late.glu"), "\"late\"").unwrap();
        })
        .unwrap_err();

    assert!(matches!(error, LoadGluonError::Enumerate { source, .. }
        if source.to_string().contains("fragment collection changed")));
}

#[test]
fn malformed_and_conversion_errors_are_visible_with_their_paths() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let malformed = temporary.path().join("dummy.d/malformed.glu");
    write(&malformed, "let value = in value");

    let error = manager.load_gluon(&Evaluator::default(), &StringCodec).unwrap_err();
    let LoadGluonError::Evaluation { path, source } = error else {
        panic!("expected evaluation error");
    };
    assert_eq!(path, malformed);
    assert_eq!(source.source_name.as_deref(), Some("dummy.d/malformed.glu"));
    assert!(source.span.is_some());

    fs::remove_file(&path).unwrap();
    let rejected = temporary.path().join("dummy.d/rejected.glu");
    write(&rejected, "\"reject\"");
    let error = manager.load_gluon(&Evaluator::default(), &StringCodec).unwrap_err();
    let LoadGluonError::Conversion { path, source } = error else {
        panic!("expected conversion error");
    };
    assert_eq!(path, rejected);
    assert!(source.to_string().contains("rejected dummy value"));
}

#[test]
fn load_gluon_never_falls_back_to_yaml_or_kdl() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    write(temporary.path().join("dummy.yaml"), "not: valid: yaml");
    write(temporary.path().join("dummy.kdl"), "not valid kdl {");
    write(temporary.path().join("dummy.d/fragment.yaml"), "\"yaml\"");
    write(temporary.path().join("dummy.d/fragment.kdl"), "\"kdl\"");

    let loaded = manager.load_gluon(&Evaluator::default(), &StringCodec).unwrap();
    assert!(loaded.is_empty());

    write(temporary.path().join("dummy.d/fragment.glu"), "\"gluon\"");
    let loaded = manager.load_gluon(&Evaluator::default(), &StringCodec).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].value, "gluon");
}

#[test]
fn fragment_relative_imports_are_contained_to_the_layer_root() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    write(temporary.path().join("dummy.d/modules/value.glu"), "\"imported\"");
    write(
        temporary.path().join("dummy.d/fragment.glu"),
        "import! \"modules/value.glu\"",
    );

    let loaded = manager.load_gluon(&Evaluator::default(), &StringCodec).unwrap();

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].value, "imported");
    assert_eq!(
        loaded[0]
            .fingerprint
            .imported_modules
            .iter()
            .map(|module| module.logical_name.as_str())
            .collect::<Vec<_>>(),
        ["dummy.d/modules/value.glu"]
    );
}

#[test]
fn save_and_delete_gluon_use_a_generated_atomic_fragment() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let value = "saved".to_owned();
    let path = manager.save_gluon("example", &value, &StringCodec).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, format!("{GENERATED_GLUON_MARKER}\"saved\"\n"));
    manager
        .save_gluon("example", &"updated".to_owned(), &StringCodec)
        .unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        format!("{GENERATED_GLUON_MARKER}\"updated\"\n")
    );
    assert_eq!(
        manager.load_gluon(&Evaluator::default(), &StringCodec).unwrap()[0].value,
        "updated"
    );
    assert_no_temporary_fragments(path.parent().unwrap());

    write(path.with_extension("yaml"), "legacy: true");
    write(path.with_extension("kdl"), "legacy #true");
    manager.delete_gluon::<String>("example").unwrap();
    assert!(!path.exists());
    assert!(path.with_extension("yaml").exists());
    assert!(path.with_extension("kdl").exists());
    manager.delete_gluon::<String>("example").unwrap();
}

#[test]
fn save_and_delete_reject_traversal_and_non_normal_names() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let value = "value".to_owned();

    for name in [
        "",
        ".",
        "..",
        "../escape",
        "nested/name",
        r"nested\name",
        "/absolute",
        "name/",
        "./name",
        "name/../other",
        "line\nbreak",
    ] {
        assert!(matches!(
            manager.save_gluon(name, &value, &StringCodec),
            Err(SaveGluonError::InvalidName { .. })
        ));
        assert!(matches!(
            manager.delete_gluon::<String>(name),
            Err(DeleteGluonError::InvalidName { .. })
        ));
    }
    let accepted = "x".repeat(MAX_GLUON_FRAGMENT_NAME_BYTES);
    let rejected = "x".repeat(MAX_GLUON_FRAGMENT_NAME_BYTES + 1);
    let accepted_path = manager.save_gluon(&accepted, &value, &StringCodec).unwrap();
    assert_eq!(accepted_path.file_name().unwrap().as_bytes().len(), 255);
    assert!(matches!(
        manager.save_gluon(&rejected, &value, &StringCodec),
        Err(SaveGluonError::InvalidName { .. })
    ));
    manager.delete_gluon::<String>(&accepted).unwrap();
    assert!(!temporary.path().join("escape.glu").exists());
}

#[test]
fn generated_output_accepts_n_bytes_and_rejects_n_plus_one() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let accepted = generated_literal_with_total_size(MAX_GENERATED_GLUON_BYTES);
    let rejected = generated_literal_with_total_size(MAX_GENERATED_GLUON_BYTES + 1);

    let path = manager.save_gluon("bounded", &accepted, &RawCodec).unwrap();
    assert_eq!(fs::metadata(path).unwrap().len(), MAX_GENERATED_GLUON_BYTES as u64);
    let error = manager.save_gluon("overflow", &rejected, &RawCodec).unwrap_err();
    assert!(matches!(
        error,
        SaveGluonError::GeneratedTooLarge {
            size,
            limit: MAX_GENERATED_GLUON_BYTES
        } if size == MAX_GENERATED_GLUON_BYTES + 1
    ));
}

#[test]
fn existing_fragment_inspection_accepts_n_bytes_and_rejects_n_plus_one() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = FragmentDirectory::open(temporary.path()).unwrap();
    let name = OsStr::new("bounded.glu");
    let path = temporary.path().join(name);
    let limit = 64;
    let mut content = GENERATED_GLUON_MARKER.as_bytes().to_vec();
    content.resize(limit, b'x');
    fs::write(&path, &content).unwrap();

    assert!(
        inspect_existing_fragment(&directory, name, limit)
            .unwrap()
            .unwrap()
            .generated
    );

    content.push(b'x');
    fs::write(path, content).unwrap();
    let error = inspect_existing_fragment(&directory, name, limit).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("65 bytes; limit is 64 bytes"));
}

#[test]
fn save_and_delete_never_follow_a_managed_fragment_symlink() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let target = temporary.path().join("dummy.d/linked.glu");
    let outside = temporary.path().join("outside.glu");
    write(&outside, &format!("{GENERATED_GLUON_MARKER}\"outside\"\n"));
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    symlink(&outside, &target).unwrap();

    assert!(matches!(
        manager.save_gluon("linked", &"replacement".to_owned(), &StringCodec),
        Err(SaveGluonError::ReadExisting { .. })
    ));
    assert!(matches!(
        manager.delete_gluon::<String>("linked"),
        Err(DeleteGluonError::ReadExisting { .. })
    ));
    assert_eq!(
        fs::read_to_string(outside).unwrap(),
        format!("{GENERATED_GLUON_MARKER}\"outside\"\n")
    );
    assert!(target.is_symlink());
}

#[test]
fn save_and_delete_reject_a_fifo_without_blocking() {
    let temporary = tempfile::tempdir().unwrap();
    let fifo = temporary.path().join("dummy.d/blocking.glu");
    fs::create_dir_all(fifo.parent().unwrap()).unwrap();
    mkfifo(&fifo);
    let save_manager = Manager::custom(temporary.path());
    let fifo_for_save = fifo.clone();
    let save = finish_nonblocking(&fifo_for_save, move || {
        save_manager.save_gluon("blocking", &"replacement".to_owned(), &StringCodec)
    });
    assert!(matches!(save, Err(SaveGluonError::ReadExisting { .. })));

    let delete_manager = Manager::custom(temporary.path());
    let fifo_for_delete = fifo.clone();
    let delete = finish_nonblocking(&fifo_for_delete, move || {
        delete_manager.delete_gluon::<String>("blocking")
    });
    assert!(matches!(delete, Err(DeleteGluonError::ReadExisting { .. })));
}

#[test]
fn a_failed_atomic_commit_removes_its_unpredictable_staging_file() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = FragmentDirectory::open(temporary.path()).unwrap();
    let name = OsStr::new("target.glu");
    let target = temporary.path().join(name);

    let error = atomic_write_with_hook(
        &directory,
        name,
        &target,
        GENERATED_GLUON_MARKER.as_bytes(),
        ExpectedTarget::Missing,
        |temporary_path| {
            let temporary_name = temporary_path.file_name().unwrap().to_string_lossy();
            let random = temporary_name.strip_prefix(TEMPORARY_PREFIX).unwrap();
            assert_eq!(random.len(), TEMPORARY_RANDOM_BYTES * 2);
            assert!(random.bytes().all(|byte| byte.is_ascii_hexdigit()));
            fs::create_dir(&target).unwrap();
        },
    )
    .unwrap_err();

    assert!(matches!(error, SaveGluonError::Rename { .. }));
    assert!(target.is_dir());
    assert_no_temporary_fragments(temporary.path());
}

#[test]
fn replacing_a_managed_directory_cannot_redirect_an_atomic_save() {
    let temporary = tempfile::tempdir().unwrap();
    let managed = temporary.path().join("managed");
    let held = temporary.path().join("held");
    fs::create_dir(&managed).unwrap();
    let directory = FragmentDirectory::open(&managed).unwrap();
    let name = OsStr::new("target.glu");
    let target = managed.join(name);

    let error = atomic_write_with_hook(
        &directory,
        name,
        &target,
        GENERATED_GLUON_MARKER.as_bytes(),
        ExpectedTarget::Missing,
        |_| {
            fs::rename(&managed, &held).unwrap();
            fs::create_dir(&managed).unwrap();
        },
    )
    .unwrap_err();

    assert!(matches!(error, SaveGluonError::ReadExisting { .. }));
    assert!(fs::read_dir(&managed).unwrap().next().is_none());
    assert!(!held.join(name).exists());
    assert_no_temporary_fragments(&held);
}

#[test]
fn generated_marker_rechecks_fail_closed_when_the_target_is_replaced() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = FragmentDirectory::open(temporary.path()).unwrap();
    let name = OsStr::new("race.glu");
    let target = temporary.path().join(name);
    fs::write(&target, format!("{GENERATED_GLUON_MARKER}\"old\"\n")).unwrap();
    let expected = inspect_existing_fragment(&directory, name, MAX_GENERATED_GLUON_BYTES)
        .unwrap()
        .unwrap()
        .identity;

    let error = atomic_write_with_hook(
        &directory,
        name,
        &target,
        format!("{GENERATED_GLUON_MARKER}\"new\"\n").as_bytes(),
        ExpectedTarget::Generated(expected),
        |_| {
            fs::remove_file(&target).unwrap();
            fs::write(&target, "\"authored replacement\"\n").unwrap();
        },
    )
    .unwrap_err();

    assert!(matches!(error, SaveGluonError::ReadExisting { .. }));
    assert_eq!(fs::read_to_string(&target).unwrap(), "\"authored replacement\"\n");
    assert_no_temporary_fragments(temporary.path());
}

#[test]
fn delete_rechecks_the_generated_inode_before_unlinking() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let target = manager
        .save_gluon("race", &"generated".to_owned(), &StringCodec)
        .unwrap();
    let outside = temporary.path().join("outside.glu");
    fs::write(&outside, "outside").unwrap();

    let error = manager
        .delete_gluon_with_hook::<String>("race", || {
            fs::remove_file(&target).unwrap();
            symlink(&outside, &target).unwrap();
        })
        .unwrap_err();

    assert!(matches!(error, DeleteGluonError::ReadExisting { .. }));
    assert!(target.is_symlink());
    assert_eq!(fs::read_to_string(outside).unwrap(), "outside");
}

#[test]
fn replacing_a_managed_directory_cannot_redirect_delete() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let target = manager
        .save_gluon("race", &"generated".to_owned(), &StringCodec)
        .unwrap();
    let managed = target.parent().unwrap().to_owned();
    let held = temporary.path().join("held");

    let error = manager
        .delete_gluon_with_hook::<String>("race", || {
            fs::rename(&managed, &held).unwrap();
            fs::create_dir(&managed).unwrap();
        })
        .unwrap_err();

    assert!(matches!(error, DeleteGluonError::ReadExisting { .. }));
    assert!(held.join("race.glu").is_file());
    assert!(!managed.join("race.glu").exists());
}

#[test]
fn save_never_overwrites_an_authored_fragment() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let path = temporary.path().join("dummy.d/authored.glu");
    write(&path, "\"user expression\"\n");

    let error = manager
        .save_gluon("authored", &"generated replacement".to_owned(), &StringCodec)
        .unwrap_err();

    assert!(matches!(error, SaveGluonError::AuthoredFragment { path: ref found } if found == &path));
    assert_eq!(fs::read_to_string(path).unwrap(), "\"user expression\"\n");
}

#[test]
fn delete_never_removes_an_authored_fragment() {
    let temporary = tempfile::tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let path = temporary.path().join("dummy.d/authored.glu");
    write(&path, "\"user expression\"\n");

    let error = manager.delete_gluon::<String>("authored").unwrap_err();

    assert!(matches!(error, DeleteGluonError::AuthoredFragment { path: ref found } if found == &path));
    assert_eq!(fs::read_to_string(path).unwrap(), "\"user expression\"\n");
}
