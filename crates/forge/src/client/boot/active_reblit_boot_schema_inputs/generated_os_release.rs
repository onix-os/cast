use std::{
    ffi::CStr,
    fs::File,
    io,
    os::{
        fd::{AsRawFd as _, BorrowedFd},
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
};

use crate::{
    linux_fs::{controlled_resolution, open_path_descriptor_readonly_until, openat2_file_until},
    state,
    transition_identity::RevalidatedActiveReblitBootStateRoots,
};

use super::{
    ActiveReblitBootSchemaFallbackReason, ActiveReblitBootSchemaInputsError, ActiveReblitBootSchemaStructuralReason,
    SchemaBudget,
};

const LIB_NAME: &CStr = c"lib";
const OS_RELEASE_NAME: &CStr = c"os-release";
const POSIX_ACCESS_ACL_XATTR: &CStr = c"system.posix_acl_access";
const POSIX_DEFAULT_ACL_XATTR: &CStr = c"system.posix_acl_default";
const MAX_INTERRUPTED_RETRIES: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryWitness {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileWitness {
    regular: bool,
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

pub(super) struct RetainedGeneratedOsRelease {
    state_id: state::Id,
    usr: File,
    usr_witness: DirectoryWitness,
    lib: File,
    lib_witness: DirectoryWitness,
    pinned: File,
    readable: File,
    file_witness: FileWitness,
    bytes: Box<[u8]>,
}

pub(super) enum GeneratedPreparation {
    Ready(RetainedGeneratedOsRelease),
    Unavailable(ActiveReblitBootSchemaFallbackReason),
}

impl RetainedGeneratedOsRelease {
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn prepare(
        state_id: state::Id,
        usr: BorrowedFd<'_>,
        budget: &mut SchemaBudget,
    ) -> Result<GeneratedPreparation, ActiveReblitBootSchemaInputsError> {
        budget.step()?;
        let usr = duplicate_descriptor(usr).map_err(|source| generated_io(state_id, "retain state /usr", source))?;
        let usr_witness =
            directory_witness(&usr).map_err(|source| generated_io(state_id, "inspect state /usr", source))?;
        let lib = match open_generated_component(
            &usr,
            LIB_NAME,
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            budget,
        ) {
            Ok(lib) => lib,
            Err(OpenGeneratedError::Structural(reason)) => return Ok(unavailable(reason)),
            Err(OpenGeneratedError::Operational(source)) => {
                return Err(generated_io(state_id, "open generated metadata lib", source));
            }
            Err(OpenGeneratedError::Boundary(source)) => return Err(source),
        };
        let lib_witness = directory_witness(&lib)
            .map_err(|source| generated_io(state_id, "inspect generated metadata lib", source))?;
        if !safe_lib_witness(usr_witness, lib_witness) {
            return Ok(unavailable(ActiveReblitBootSchemaStructuralReason::UnsafeLib));
        }
        if let Some(reason) = audit_xattrs(&lib, true, budget)
            .map_err(|source| generated_io(state_id, "audit generated metadata lib attributes", source))?
        {
            return Ok(unavailable(reason));
        }

        let pinned = match open_generated_component(
            &lib,
            OS_RELEASE_NAME,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            budget,
        ) {
            Ok(file) => file,
            Err(OpenGeneratedError::Structural(reason)) => return Ok(unavailable(reason)),
            Err(OpenGeneratedError::Operational(source)) => {
                return Err(generated_io(state_id, "retain generated os-release", source));
            }
            Err(OpenGeneratedError::Boundary(source)) => return Err(source),
        };
        let pinned_witness =
            file_witness(&pinned).map_err(|source| generated_io(state_id, "inspect generated os-release", source))?;
        if !safe_file_witness(lib_witness, pinned_witness, budget.policy.max_source_bytes) {
            let reason = if pinned_witness.length > budget.policy.max_source_bytes as u64 {
                ActiveReblitBootSchemaStructuralReason::SourceTooLarge
            } else {
                ActiveReblitBootSchemaStructuralReason::UnsafeOsRelease
            };
            return Ok(unavailable(reason));
        }
        let length = usize::try_from(pinned_witness.length).expect("policy-bounded schema length fits usize");
        budget.admit_source_bytes(length)?;
        budget.step()?;
        let readable = open_path_descriptor_readonly_until(&pinned, budget.deadline)
            .map_err(|source| generated_io(state_id, "open retained os-release for reading", source))?;
        if file_witness(&readable).map_err(|source| generated_io(state_id, "inspect readable os-release", source))?
            != pinned_witness
        {
            return Ok(changed_generated());
        }
        if let Some(reason) = audit_xattrs(&readable, false, budget)
            .map_err(|source| generated_io(state_id, "audit generated os-release attributes", source))?
        {
            return Ok(unavailable(reason));
        }
        let bytes = read_exact_file(&readable, length, budget)
            .map_err(|source| generated_io(state_id, "read generated os-release", source))?
            .into_boxed_slice();
        after_generated_read(state_id);
        if !stable_generated_names(
            state_id,
            &usr,
            usr_witness,
            &lib,
            lib_witness,
            &pinned,
            &readable,
            pinned_witness,
            budget,
        )? {
            return Ok(changed_generated());
        }
        Ok(GeneratedPreparation::Ready(Self {
            state_id,
            usr,
            usr_witness,
            lib,
            lib_witness,
            pinned,
            readable,
            file_witness: pinned_witness,
            bytes,
        }))
    }

    pub(super) fn revalidate(
        &self,
        roots: &RevalidatedActiveReblitBootStateRoots<'_>,
        budget: &mut SchemaBudget,
    ) -> Result<(), ActiveReblitBootSchemaInputsError> {
        let root = roots.roots().find(|root| root.state_id() == self.state_id).ok_or(
            ActiveReblitBootSchemaInputsError::MissingEligibleRoot {
                state: i32::from(self.state_id),
            },
        )?;
        let current_usr = duplicate_descriptor(root.usr())
            .map_err(|source| generated_io(self.state_id, "retain revalidated state /usr", source))?;
        let current_usr_witness = directory_witness(&current_usr)
            .map_err(|source| generated_io(self.state_id, "inspect revalidated state /usr", source))?;
        if current_usr_witness != self.usr_witness
            || directory_witness(&self.usr)
                .map_err(|source| generated_io(self.state_id, "inspect retained state /usr", source))?
                != self.usr_witness
        {
            return Err(source_changed(self.state_id));
        }
        if !stable_generated_names(
            self.state_id,
            &self.usr,
            self.usr_witness,
            &self.lib,
            self.lib_witness,
            &self.pinned,
            &self.readable,
            self.file_witness,
            budget,
        )? {
            return Err(source_changed(self.state_id));
        }
        audit_xattrs(&self.lib, true, budget)
            .map_err(|source| generated_io(self.state_id, "reaudit generated metadata lib", source))?
            .map_or(Ok(()), |_| Err(source_changed(self.state_id)))?;
        audit_xattrs(&self.readable, false, budget)
            .map_err(|source| generated_io(self.state_id, "reaudit generated os-release", source))?
            .map_or(Ok(()), |_| Err(source_changed(self.state_id)))?;
        let bytes = read_exact_file(&self.readable, self.bytes.len(), budget)
            .map_err(|source| generated_io(self.state_id, "reread generated os-release", source))?;
        after_generated_revalidation_read(self.state_id);
        if bytes.as_slice() != self.bytes.as_ref()
            || file_witness(&self.readable)
                .map_err(|source| generated_io(self.state_id, "reinspect generated os-release", source))?
                != self.file_witness
        {
            return Err(source_changed(self.state_id));
        }
        if !stable_generated_names(
            self.state_id,
            &self.usr,
            self.usr_witness,
            &self.lib,
            self.lib_witness,
            &self.pinned,
            &self.readable,
            self.file_witness,
            budget,
        )? {
            return Err(source_changed(self.state_id));
        }
        Ok(())
    }
}

fn unavailable(reason: ActiveReblitBootSchemaStructuralReason) -> GeneratedPreparation {
    GeneratedPreparation::Unavailable(ActiveReblitBootSchemaFallbackReason::Structural(reason))
}

fn changed_generated() -> GeneratedPreparation {
    unavailable(ActiveReblitBootSchemaStructuralReason::ChangedDuringRead)
}

#[allow(clippy::too_many_arguments)]
fn stable_generated_names(
    state_id: state::Id,
    usr: &File,
    usr_witness: DirectoryWitness,
    lib: &File,
    lib_witness: DirectoryWitness,
    pinned: &File,
    readable: &File,
    expected: FileWitness,
    budget: &mut SchemaBudget,
) -> Result<bool, ActiveReblitBootSchemaInputsError> {
    budget.step()?;
    if directory_witness(usr).map_err(|source| generated_io_raw("reinspect retained /usr", source))? != usr_witness
        || directory_witness(lib).map_err(|source| generated_io_raw("reinspect retained lib", source))? != lib_witness
        || file_witness(pinned).map_err(|source| generated_io_raw("reinspect pinned os-release", source))? != expected
        || file_witness(readable).map_err(|source| generated_io_raw("reinspect readable os-release", source))?
            != expected
    {
        return Ok(false);
    }
    let named_lib = match open_generated_component(
        usr,
        LIB_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        budget,
    ) {
        Ok(file) => file,
        Err(OpenGeneratedError::Structural(_)) => return Ok(false),
        Err(OpenGeneratedError::Operational(source)) => {
            return Err(generated_io_raw("reopen generated metadata lib", source));
        }
        Err(OpenGeneratedError::Boundary(source)) => return Err(source),
    };
    if directory_witness(&named_lib).map_err(|source| generated_io_raw("inspect named metadata lib", source))?
        != lib_witness
    {
        return Ok(false);
    }
    after_generated_name_lib_open(state_id);
    let named = match open_generated_component(
        &named_lib,
        OS_RELEASE_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        budget,
    ) {
        Ok(file) => file,
        Err(OpenGeneratedError::Structural(_)) => return Ok(false),
        Err(OpenGeneratedError::Operational(source)) => {
            return Err(generated_io_raw("reopen generated os-release", source));
        }
        Err(OpenGeneratedError::Boundary(source)) => return Err(source),
    };
    if file_witness(&named).map_err(|source| generated_io_raw("inspect named os-release", source))? != expected {
        return Ok(false);
    }
    after_generated_name_file_open(state_id);
    let final_named_lib = match open_generated_component(
        usr,
        LIB_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        budget,
    ) {
        Ok(file) => file,
        Err(OpenGeneratedError::Structural(_)) => return Ok(false),
        Err(OpenGeneratedError::Operational(source)) => {
            return Err(generated_io_raw("finally reopen generated metadata lib", source));
        }
        Err(OpenGeneratedError::Boundary(source)) => return Err(source),
    };
    if directory_witness(&final_named_lib)
        .map_err(|source| generated_io_raw("finally inspect named metadata lib", source))?
        != lib_witness
    {
        return Ok(false);
    }
    let final_named = match open_generated_component(
        &final_named_lib,
        OS_RELEASE_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        budget,
    ) {
        Ok(file) => file,
        Err(OpenGeneratedError::Structural(_)) => return Ok(false),
        Err(OpenGeneratedError::Operational(source)) => {
            return Err(generated_io_raw("finally reopen generated os-release", source));
        }
        Err(OpenGeneratedError::Boundary(source)) => return Err(source),
    };
    if file_witness(&final_named).map_err(|source| generated_io_raw("finally inspect named os-release", source))?
        != expected
    {
        return Ok(false);
    }
    budget.step()?;
    Ok(
        directory_witness(usr).map_err(|source| generated_io_raw("finally inspect retained /usr", source))?
            == usr_witness
            && directory_witness(lib).map_err(|source| generated_io_raw("finally inspect retained lib", source))?
                == lib_witness
            && directory_witness(&named_lib).map_err(|source| generated_io_raw("finally inspect named lib", source))?
                == lib_witness
            && file_witness(pinned).map_err(|source| generated_io_raw("finally inspect pinned os-release", source))?
                == expected
            && file_witness(readable)
                .map_err(|source| generated_io_raw("finally inspect readable os-release", source))?
                == expected
            && file_witness(&named).map_err(|source| generated_io_raw("finally inspect named os-release", source))?
                == expected
            && directory_witness(&final_named_lib)
                .map_err(|source| generated_io_raw("finally reinspect named metadata lib", source))?
                == lib_witness
            && file_witness(&final_named)
                .map_err(|source| generated_io_raw("finally reinspect named os-release", source))?
                == expected,
    )
}

enum OpenGeneratedError {
    Structural(ActiveReblitBootSchemaStructuralReason),
    Operational(io::Error),
    Boundary(ActiveReblitBootSchemaInputsError),
}

fn open_generated_component(
    parent: &File,
    name: &CStr,
    flags: i32,
    budget: &mut SchemaBudget,
) -> Result<File, OpenGeneratedError> {
    budget.step().map_err(OpenGeneratedError::Boundary)?;
    #[cfg(test)]
    if let Some(source) = take_generated_operational_fault() {
        return Err(OpenGeneratedError::Operational(source));
    }
    openat2_file_until(
        parent.as_raw_fd(),
        name,
        flags,
        0,
        controlled_resolution(),
        budget.deadline,
    )
    .map_err(|source| match source.raw_os_error() {
        Some(nix::libc::ENOENT) if name == LIB_NAME => {
            OpenGeneratedError::Structural(ActiveReblitBootSchemaStructuralReason::MissingLib)
        }
        Some(nix::libc::ENOENT) => {
            OpenGeneratedError::Structural(ActiveReblitBootSchemaStructuralReason::MissingOsRelease)
        }
        Some(nix::libc::ENOTDIR | nix::libc::ELOOP | nix::libc::EXDEV) if name == LIB_NAME => {
            OpenGeneratedError::Structural(ActiveReblitBootSchemaStructuralReason::UnsafeLib)
        }
        Some(nix::libc::ENOTDIR | nix::libc::ELOOP | nix::libc::EXDEV) => {
            OpenGeneratedError::Structural(ActiveReblitBootSchemaStructuralReason::UnsafeOsRelease)
        }
        _ => OpenGeneratedError::Operational(source),
    })
}

fn duplicate_descriptor(descriptor: BorrowedFd<'_>) -> io::Result<File> {
    descriptor.try_clone_to_owned().map(File::from)
}

fn directory_witness(file: &File) -> io::Result<DirectoryWitness> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained schema root is not a directory",
        ));
    }
    Ok(DirectoryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        group: metadata.gid(),
        mode: metadata.permissions().mode() & 0o7777,
        links: metadata.nlink(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

fn file_witness(file: &File) -> io::Result<FileWitness> {
    let metadata = file.metadata()?;
    Ok(FileWitness {
        regular: metadata.file_type().is_file(),
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        group: metadata.gid(),
        mode: metadata.permissions().mode() & 0o7777,
        links: metadata.nlink(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

fn safe_lib_witness(usr: DirectoryWitness, lib: DirectoryWitness) -> bool {
    lib.device == usr.device && lib.owner == usr.owner && lib.owner == effective_user_id() && lib.mode == 0o755
}

fn safe_file_witness(lib: DirectoryWitness, file: FileWitness, max_source_bytes: usize) -> bool {
    file.regular
        && file.device == lib.device
        && file.owner == lib.owner
        && file.owner == effective_user_id()
        && file.mode == 0o644
        && file.links == 1
        && file.length <= max_source_bytes as u64
}

fn effective_user_id() -> u32 {
    // SAFETY: geteuid takes no arguments and cannot fail.
    unsafe { nix::libc::geteuid() }
}

fn audit_xattrs(
    file: &File,
    directory: bool,
    budget: &mut SchemaBudget,
) -> Result<Option<ActiveReblitBootSchemaStructuralReason>, io::Error> {
    if probe_xattr(file, POSIX_ACCESS_ACL_XATTR, budget)?.is_some() {
        return Ok(Some(ActiveReblitBootSchemaStructuralReason::AccessAcl));
    }
    if directory && probe_xattr(file, POSIX_DEFAULT_ACL_XATTR, budget)?.is_some() {
        return Ok(Some(ActiveReblitBootSchemaStructuralReason::DefaultAcl));
    }
    let names = retry_bounded_syscall(budget, || {
        // SAFETY: `file` remains live and a null size probe writes no bytes.
        let result = unsafe { nix::libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0) };
        if result >= 0 {
            Ok(result as usize)
        } else {
            Err(io::Error::last_os_error())
        }
    });
    match names {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(ActiveReblitBootSchemaStructuralReason::ExtendedAttributes)),
        Err(source) if source.raw_os_error() == Some(nix::libc::EOPNOTSUPP) => Ok(None),
        Err(source) => Err(source),
    }
}

fn probe_xattr(file: &File, name: &CStr, budget: &mut SchemaBudget) -> Result<Option<usize>, io::Error> {
    let result = retry_bounded_syscall(budget, || {
        // SAFETY: `file` and `name` remain live and a null size probe writes no bytes.
        let result = unsafe { nix::libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if result >= 0 {
            Ok(result as usize)
        } else {
            Err(io::Error::last_os_error())
        }
    });
    match result {
        Ok(length) => Ok(Some(length)),
        Err(source) if matches!(source.raw_os_error(), Some(nix::libc::ENODATA | nix::libc::EOPNOTSUPP)) => Ok(None),
        Err(source) => Err(source),
    }
}

fn retry_bounded_syscall<T>(budget: &mut SchemaBudget, mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut interruptions = 0usize;
    loop {
        budget
            .step()
            .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error.to_string()))?;
        match operation() {
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {
                if interruptions >= MAX_INTERRUPTED_RETRIES {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "schema metadata syscall exceeded its interruption limit",
                    ));
                }
                interruptions += 1;
            }
            result => return result,
        }
    }
}

pub(super) fn read_exact_descriptor(
    descriptor: BorrowedFd<'_>,
    length: usize,
    budget: &mut SchemaBudget,
) -> io::Result<Vec<u8>> {
    read_exact_raw(descriptor.as_raw_fd(), length, budget)
}

fn read_exact_file(file: &File, length: usize, budget: &mut SchemaBudget) -> io::Result<Vec<u8>> {
    read_exact_raw(file.as_raw_fd(), length, budget)
}

fn read_exact_raw(fd: i32, length: usize, budget: &mut SchemaBudget) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0_u8; length];
    let mut offset = 0usize;
    let mut interruptions = 0usize;
    while offset < length {
        budget
            .step()
            .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error.to_string()))?;
        // SAFETY: `fd` remains borrowed, the remaining buffer is writable, and
        // the bounded offset is representable after the checked conversion.
        let result = unsafe {
            nix::libc::pread(
                fd,
                bytes[offset..].as_mut_ptr().cast(),
                bytes.len() - offset,
                i64::try_from(offset).expect("schema byte limit fits off_t"),
            )
        };
        if result > 0 {
            offset += result as usize;
            interruptions = 0;
        } else if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "schema source shortened during retained read",
            ));
        } else {
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::Interrupted {
                return Err(source);
            }
            if interruptions >= MAX_INTERRUPTED_RETRIES {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "schema read exceeded its interruption limit",
                ));
            }
            interruptions += 1;
        }
    }
    Ok(bytes)
}

fn generated_io(state_id: state::Id, operation: &'static str, source: io::Error) -> ActiveReblitBootSchemaInputsError {
    ActiveReblitBootSchemaInputsError::GeneratedIo {
        state: i32::from(state_id),
        operation,
        source,
    }
}

fn generated_io_raw(operation: &'static str, source: io::Error) -> ActiveReblitBootSchemaInputsError {
    ActiveReblitBootSchemaInputsError::GeneratedRevalidationIo { operation, source }
}

fn source_changed(state_id: state::Id) -> ActiveReblitBootSchemaInputsError {
    ActiveReblitBootSchemaInputsError::GeneratedSourceChanged {
        state: i32::from(state_id),
    }
}

#[cfg(test)]
std::thread_local! {
    static AFTER_GENERATED_READ: std::cell::RefCell<Option<Box<dyn FnOnce(state::Id)>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_GENERATED_NAME_LIB_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce(state::Id)>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_GENERATED_NAME_FILE_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce(state::Id)>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_GENERATED_REVALIDATION_READ: std::cell::RefCell<Option<Box<dyn FnOnce(state::Id)>>> =
        const { std::cell::RefCell::new(None) };
    static GENERATED_OPERATIONAL_FAULT: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_after_generated_read(hook: impl FnOnce(state::Id) + 'static) {
    AFTER_GENERATED_READ.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "schema read hook already armed"
        );
    });
}

fn after_generated_read(_state_id: state::Id) {
    #[cfg(test)]
    AFTER_GENERATED_READ.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(_state_id);
        }
    });
}

#[cfg(test)]
pub(super) fn arm_after_generated_name_lib_open(hook: impl FnOnce(state::Id) + 'static) {
    AFTER_GENERATED_NAME_LIB_OPEN.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "generated lib-name hook already armed"
        );
    });
}

fn after_generated_name_lib_open(_state_id: state::Id) {
    #[cfg(test)]
    AFTER_GENERATED_NAME_LIB_OPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(_state_id);
        }
    });
}

#[cfg(test)]
pub(super) fn arm_after_generated_name_file_open(hook: impl FnOnce(state::Id) + 'static) {
    AFTER_GENERATED_NAME_FILE_OPEN.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "generated file-name hook already armed"
        );
    });
}

fn after_generated_name_file_open(_state_id: state::Id) {
    #[cfg(test)]
    AFTER_GENERATED_NAME_FILE_OPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(_state_id);
        }
    });
}

#[cfg(test)]
pub(super) fn arm_after_generated_revalidation_read(hook: impl FnOnce(state::Id) + 'static) {
    AFTER_GENERATED_REVALIDATION_READ.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "generated revalidation-read hook already armed"
        );
    });
}

fn after_generated_revalidation_read(_state_id: state::Id) {
    #[cfg(test)]
    AFTER_GENERATED_REVALIDATION_READ.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(_state_id);
        }
    });
}

#[cfg(test)]
pub(super) fn arm_generated_operational_fault(errno: i32) {
    GENERATED_OPERATIONAL_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(errno)).is_none(),
            "schema operational fault already armed"
        );
    });
}

#[cfg(test)]
fn take_generated_operational_fault() -> Option<io::Error> {
    GENERATED_OPERATIONAL_FAULT.with(|slot| slot.take().map(io::Error::from_raw_os_error))
}
