//! Harness-free proof for the production legacy-clone boundary.
//!
//! Rust's ordinary test harness owns a thread pool, so a unit test cannot
//! truthfully demonstrate the production single-task precondition. This
//! executable deliberately opts out of libtest, starts with exactly one task,
//! proves a parked second task is rejected before clone, then exercises both a
//! successful payload and a contained panic from the single-task state.

use std::ffi::CString;
use std::io;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::process;
use std::sync::mpsc;

use container::{
    Container, DevPolicy, Error, LoopbackPolicy, ProcPolicy, PseudoFilesystemPolicy, SysPolicy, TmpPolicy,
};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::{errno::Errno, libc};

fn main() {
    assert_exact_main_task("standalone startup");
    prove_parked_task_is_rejected();
    assert_exact_main_task("after parked-task rejection");
    prove_nonwaitable_sigchld_dispositions_are_rejected();
    assert_exact_main_task("after SIGCHLD-disposition rejection");

    let success_root = tempfile::tempdir().expect("create successful activation root");
    let success_anchor = open_path_directory(success_root.path());
    let success = minimal_container(success_root.path(), &success_anchor)
        .run::<io::Error>(|| std::fs::write("/single-task-witness", b"production legacy clone"));
    match success {
        Ok(()) => assert_eq!(
            std::fs::read(success_root.path().join("single-task-witness")).expect("read payload witness"),
            b"production legacy clone"
        ),
        Err(error) if namespace_activation_unavailable(&error) => {
            eprintln!("SKIP standalone legacy activation: host denied required namespaces: {error}");
            return;
        }
        Err(error) => panic!("single-task legacy activation failed: {error}"),
    }

    assert_exact_main_task("after successful activation");
    let panic_root = tempfile::tempdir().expect("create panic activation root");
    let panic_anchor = open_path_directory(panic_root.path());
    let panic_result = minimal_container(panic_root.path(), &panic_anchor)
        .run::<io::Error>(|| panic!("standalone payload panic must remain inside the raw child"));
    match panic_result {
        Err(Error::Failure { message }) => assert_eq!(
            message,
            "raw fork-like clone child panicked; payload setup was aborted before returning through the cloned parent stack"
        ),
        other => panic!("panicking legacy payload was not contained as a child failure: {other:?}"),
    }
    assert_exact_main_task("after contained payload panic");
}

fn prove_nonwaitable_sigchld_dispositions_are_rejected() {
    extern "C" fn custom_sigchld_handler(_: libc::c_int) {}

    for (case, action, expected) in [
        (
            "SIG_IGN",
            SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty()),
            "found SIG_IGN",
        ),
        (
            "SA_NOCLDWAIT",
            SigAction::new(SigHandler::SigDfl, SaFlags::SA_NOCLDWAIT, SigSet::empty()),
            "without SA_NOCLDWAIT",
        ),
        (
            "custom handler",
            SigAction::new(
                SigHandler::Handler(custom_sigchld_handler),
                SaFlags::empty(),
                SigSet::empty(),
            ),
            "found a custom handler",
        ),
    ] {
        // SAFETY: each action is fully initialized. The standalone process is
        // exactly single-tasked, and the prior action is restored immediately
        // after the production pre-clone audit returns.
        let previous = unsafe { sigaction(Signal::SIGCHLD, &action) }
            .unwrap_or_else(|source| panic!("install isolated {case} SIGCHLD action: {source}"));
        let root = tempfile::tempdir().expect("create SIGCHLD rejection activation root");
        let anchor = open_path_directory(root.path());
        let result = minimal_container(root.path(), &anchor).run::<io::Error>(|| Ok(()));
        // SAFETY: previous was returned by sigaction for this exact signal.
        unsafe { sigaction(Signal::SIGCHLD, &previous) }
            .unwrap_or_else(|source| panic!("restore SIGCHLD after {case} rejection: {source}"));

        match result {
            Err(Error::Failure { message }) => assert!(
                message.starts_with(
                    "legacy clone requires a waitable SIGCHLD disposition before numeric child supervision:"
                ) && message.contains(expected),
                "unexpected production SIGCHLD diagnostic for {case}: {message}"
            ),
            other => panic!("legacy clone did not reject {case} before activation: {other:?}"),
        }
    }
}

fn prove_parked_task_is_rejected() {
    let root = tempfile::tempdir().expect("create rejection activation root");
    let anchor = open_path_directory(root.path());
    let container = minimal_container(root.path(), &anchor);
    let (ready_sender, ready_receiver) = mpsc::sync_channel(0);
    let (release_sender, release_receiver) = mpsc::sync_channel::<()>(0);
    let parked = std::thread::spawn(move || {
        // SAFETY: gettid takes no arguments and returns this kernel task ID.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        let _ = ready_sender.send(tid);
        let _ = release_receiver.recv();
    });

    let parked_tid = ready_receiver.recv().expect("parked task reports readiness");
    let observed = numeric_task_ids();
    let result = container.run::<io::Error>(|| Ok(()));
    drop(release_sender);
    parked.join().expect("parked task exits cleanly");

    assert_eq!(
        observed.len(),
        2,
        "standalone rejection proof observed tasks {observed:?}"
    );
    assert!(
        observed.contains(&parked_tid),
        "standalone rejection proof did not observe parked task {parked_tid}: {observed:?}"
    );
    match result {
        Err(Error::Failure { message }) => assert!(
            message.starts_with("legacy clone requires an authenticated single-task supervisor:")
                && message.contains("exactly single-threaded supervisor"),
            "unexpected production task-audit diagnostic: {message}"
        ),
        other => panic!("legacy clone did not reject a real parked task before activation: {other:?}"),
    }
}

fn minimal_container(root: &Path, anchor: &OwnedFd) -> Container {
    Container::new_anchored(root, anchor)
        .expect("construct anchored container")
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Disabled,
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        })
        .loopback(LoopbackPolicy::KernelDefault)
}

fn open_path_directory(path: &Path) -> OwnedFd {
    let path = CString::new(path.as_os_str().as_bytes()).expect("temporary path has no NUL");
    // SAFETY: path is NUL-terminated and the flags request a descriptor-only
    // directory capability without following the final component.
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    assert!(descriptor >= 0, "open O_PATH directory: {}", io::Error::last_os_error());
    // SAFETY: successful open returned one fresh owned descriptor.
    unsafe { OwnedFd::from_raw_fd(descriptor) }
}

fn numeric_task_ids() -> Vec<libc::c_long> {
    let task_directory = format!("/proc/{}/task", process::id());
    let mut tasks = std::fs::read_dir(&task_directory)
        .unwrap_or_else(|source| panic!("enumerate {task_directory}: {source}"))
        .map(|entry| {
            let entry = entry.unwrap_or_else(|source| panic!("read {task_directory} entry: {source}"));
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<libc::c_long>().ok())
                .unwrap_or_else(|| panic!("non-numeric task entry in {task_directory}"))
        })
        .collect::<Vec<_>>();
    tasks.sort_unstable();
    tasks
}

fn assert_exact_main_task(context: &str) {
    let tasks = numeric_task_ids();
    assert_eq!(
        tasks,
        [libc::c_long::from(process::id())],
        "{context} was not an exact single-task process"
    );
}

fn namespace_activation_unavailable(error: &Error) -> bool {
    matches!(
        error,
        Error::CloneNamespaces {
            source: Errno::EPERM | Errno::EACCES | Errno::ENOSYS
        }
    ) || matches!(
        error,
        Error::Failure { message }
            if message.starts_with("clear inherited supplementary groups:")
                && message.contains("EPERM: Operation not permitted")
    )
}
