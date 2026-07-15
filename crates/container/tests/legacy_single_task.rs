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
use nix::libc;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutionRequirement {
    Optional,
    Required,
}

fn main() {
    let execution_requirement = execution_requirement_from_env();
    assert_exact_main_task("standalone startup");
    prove_parked_task_is_rejected();
    assert_exact_main_task("after parked-task rejection");
    prove_nonwaitable_sigchld_dispositions_are_rejected();
    assert_exact_main_task("after SIGCHLD-disposition rejection");

    let success_root = tempfile::tempdir().expect("create successful activation root");
    let success_anchor = open_path_directory(success_root.path());
    let success = minimal_container(success_root.path(), &success_anchor).run::<io::Error>(|| {
        require_supplementary_group_mutation_blocked()?;
        read_payload_credentials()?.require_isolated()?;
        std::fs::write(
            "/single-task-credentials",
            b"uid=0/0/0/0 gid=0/0/0/0 supplementary=0 setgroups=EPERM",
        )?;
        std::fs::write("/single-task-witness", b"production legacy clone")
    });
    match success {
        Ok(()) => {
            assert_eq!(
                std::fs::read(success_root.path().join("single-task-witness")).expect("read payload witness"),
                b"production legacy clone"
            );
            assert_eq!(
                std::fs::read(success_root.path().join("single-task-credentials"))
                    .expect("read payload credential witness"),
                b"uid=0/0/0/0 gid=0/0/0/0 supplementary=0 setgroups=EPERM"
            );
        }
        Err(error) if namespace_activation_unavailable(&error) => match execution_requirement {
            ExecutionRequirement::Optional => {
                eprintln!("SKIP standalone legacy activation: host denied required namespaces: {error}");
                return;
            }
            ExecutionRequirement::Required => panic!(
                "required execution-capability preflight failed: the host denied production container user/mount namespace setup; enable unprivileged user namespaces and permit isolated setgroups and mount setup for the delegated service: {}",
                error_chain(&error)
            ),
        },
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

#[derive(Debug, Eq, PartialEq)]
struct PayloadCredentials {
    real_uid: u32,
    effective_uid: u32,
    saved_uid: u32,
    filesystem_uid: u32,
    real_gid: u32,
    effective_gid: u32,
    saved_gid: u32,
    filesystem_gid: u32,
    supplementary_group_count: usize,
}

impl PayloadCredentials {
    fn require_isolated(&self) -> io::Result<()> {
        let isolated = Self {
            real_uid: 0,
            effective_uid: 0,
            saved_uid: 0,
            filesystem_uid: 0,
            real_gid: 0,
            effective_gid: 0,
            saved_gid: 0,
            filesystem_gid: 0,
            supplementary_group_count: 0,
        };
        if self == &isolated {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "confined payload retained non-isolated credential slots: {self:?}"
            )))
        }
    }
}

fn read_payload_credentials() -> io::Result<PayloadCredentials> {
    let mut real_uid = 0;
    let mut effective_uid = 0;
    let mut saved_uid = 0;
    // SAFETY: getresuid writes one uid_t to each valid local pointer.
    require_syscall_success(
        unsafe {
            libc::syscall(
                libc::SYS_getresuid,
                &mut real_uid as *mut libc::uid_t,
                &mut effective_uid as *mut libc::uid_t,
                &mut saved_uid as *mut libc::uid_t,
            )
        },
        "read confined real, effective, and saved-set UIDs",
    )?;
    // SAFETY: Linux returns the prior filesystem UID. The all-ones argument is
    // unmapped in this namespace, so the attempted change cannot succeed.
    let filesystem_uid = u32::try_from(unsafe { libc::syscall(libc::SYS_setfsuid, u32::MAX) })
        .map_err(|_| io::Error::last_os_error())?;

    let mut real_gid = 0;
    let mut effective_gid = 0;
    let mut saved_gid = 0;
    // SAFETY: getresgid writes one gid_t to each valid local pointer.
    require_syscall_success(
        unsafe {
            libc::syscall(
                libc::SYS_getresgid,
                &mut real_gid as *mut libc::gid_t,
                &mut effective_gid as *mut libc::gid_t,
                &mut saved_gid as *mut libc::gid_t,
            )
        },
        "read confined real, effective, and saved-set GIDs",
    )?;
    // SAFETY: Linux returns the prior filesystem GID. The all-ones argument is
    // unmapped even though the distinct auxiliary GID is also mapped, so the
    // attempted change cannot succeed.
    let filesystem_gid = u32::try_from(unsafe { libc::syscall(libc::SYS_setfsgid, u32::MAX) })
        .map_err(|_| io::Error::last_os_error())?;

    // SAFETY: a zero-sized getgroups query does not dereference its null list
    // and returns the exact supplementary-group count.
    let supplementary_group_count =
        usize::try_from(unsafe { libc::syscall(libc::SYS_getgroups, 0_usize, std::ptr::null_mut::<libc::gid_t>()) })
            .map_err(|_| io::Error::last_os_error())?;

    Ok(PayloadCredentials {
        real_uid,
        effective_uid,
        saved_uid,
        filesystem_uid,
        real_gid,
        effective_gid,
        saved_gid,
        filesystem_gid,
        supplementary_group_count,
    })
}

fn require_supplementary_group_mutation_blocked() -> io::Result<()> {
    // SAFETY: a zero-sized setgroups call does not dereference its null list.
    // This runs inside the payload only after all namespace capabilities have
    // been dropped and the mandatory seccomp policy has been installed.
    let result = unsafe { libc::syscall(libc::SYS_setgroups, 0_usize, std::ptr::null::<libc::gid_t>()) };
    if result != -1 {
        return Err(io::Error::other(
            "confined payload unexpectedly retained authority to mutate supplementary groups",
        ));
    }
    let source = io::Error::last_os_error();
    if source.raw_os_error() != Some(libc::EPERM) {
        return Err(io::Error::other(format!(
            "confined payload setgroups failed with {source}, expected EPERM"
        )));
    }
    Ok(())
}

fn require_syscall_success(result: libc::c_long, operation: &str) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else if result == -1 {
        let source = io::Error::last_os_error();
        Err(io::Error::new(source.kind(), format!("{operation}: {source}")))
    } else {
        Err(io::Error::other(format!(
            "{operation} returned unexpected nonzero result {result}"
        )))
    }
}

fn execution_requirement_from_env() -> ExecutionRequirement {
    match std::env::var("CAST_REQUIRE_EXECUTION") {
        Err(std::env::VarError::NotPresent) => ExecutionRequirement::Optional,
        Ok(value) if value == "0" => ExecutionRequirement::Optional,
        Ok(value) if value == "1" => ExecutionRequirement::Required,
        Ok(value) => panic!("CAST_REQUIRE_EXECUTION must be exactly `0` or `1`, found {value:?}"),
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("CAST_REQUIRE_EXECUTION must be the UTF-8 value `0` or `1`")
        }
    }
}

fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut messages = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        messages.push(error.to_string());
        source = error.source();
    }
    messages.join(": ")
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
    error.execution_capability_unavailable()
}
