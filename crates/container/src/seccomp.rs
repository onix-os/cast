use std::io;

use nix::libc;

// Classic BPF instruction encodings from linux/filter.h. Keeping these local
// makes the policy independent of which optional filter constants the libc
// crate happens to expose.
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_ALU_AND_K: u16 = 0x54;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_JMP_JGT_K: u16 = 0x25;
const BPF_JMP_JGE_K: u16 = 0x35;
const BPF_RET_K: u16 = 0x06;

// Offsets in Linux's `struct seccomp_data`.
const SECCOMP_DATA_NR_OFFSET: u32 = 0;
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
const SECCOMP_DATA_ARG0_LOW_OFFSET: u32 = 16;
const SECCOMP_DATA_ARG0_HIGH_OFFSET: u32 = 20;

// `AUDIT_ARCH_X86_64` from linux/audit.h. The architecture check must happen
// before inspecting the syscall number because syscall tables are
// architecture-specific.
const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
const X32_SYSCALL_BIT: u32 = 0x4000_0000;
const MAX_KNOWN_SYSCALL: u32 = 471;

const NR_CLONE: u32 = 56;
const NR_MKNOD: u32 = 133;
const NR_PIVOT_ROOT: u32 = 155;
const NR_CHROOT: u32 = 161;
const NR_MOUNT: u32 = 165;
const NR_UMOUNT2: u32 = 166;
const NR_MKNODAT: u32 = 259;
const NR_UNSHARE: u32 = 272;
const NR_OPEN_BY_HANDLE_AT: u32 = 304;
const NR_SETNS: u32 = 308;
const NR_OPEN_TREE: u32 = 428;
const NR_MOVE_MOUNT: u32 = 429;
const NR_FSOPEN: u32 = 430;
const NR_FSCONFIG: u32 = 431;
const NR_FSMOUNT: u32 = 432;
const NR_FSPICK: u32 = 433;
const NR_CLONE3: u32 = 435;
const NR_MOUNT_SETATTR: u32 = 442;
const NR_OPEN_TREE_ATTR: u32 = 467;

// Legacy clone(2) multiplexes its behavior through a 64-bit flags argument.
// Every low-word bit outside this mask is rejected, as is every high-word bit.
const SAFE_CLONE_FLAGS: u32 = 0x81fd_ff7f;
const UNSAFE_CLONE_FLAGS: u32 = !SAFE_CLONE_FLAGS;

const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const ACTION_EPERM: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;
const ACTION_ENOSYS: u32 = SECCOMP_RET_ERRNO | libc::ENOSYS as u32;

const SECCOMP_SET_MODE_FILTER: libc::c_uint = 1;
const SECCOMP_FILTER_FLAG_TSYNC: libc::c_uint = 1;
const SECCOMP_MODE_FILTER: libc::c_int = 2;

const FILTER_LEN: usize = 35;
const EPERM_INDEX: usize = 32;
const ENOSYS_INDEX: usize = 33;
const ALLOW_INDEX: usize = 34;

const fn statement(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt: 0, jf: 0, k }
}

const fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

const fn forward_offset(from: usize, to: usize) -> u8 {
    assert!(to > from, "classic BPF jumps must be forward");
    let offset = to - from - 1;
    assert!(offset <= u8::MAX as usize, "classic BPF jump is too far");
    offset as u8
}

// The filter deliberately defaults to ALLOW only after validating the ABI and
// bounding the known syscall table. Syscalls added after the audited table
// return ENOSYS, while x32 and foreign architectures are killed because their
// argument layouts cannot be interpreted safely by this policy.
const PAYLOAD_FILTER: [libc::sock_filter; FILTER_LEN] = [
    // 0: Reject every architecture except native x86_64.
    statement(BPF_LD_W_ABS, SECCOMP_DATA_ARCH_OFFSET),
    jump(
        BPF_JMP_JEQ_K,
        AUDIT_ARCH_X86_64,
        forward_offset(1, 3),
        forward_offset(1, 2),
    ),
    statement(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
    // 3: Reject x32 before applying the upper syscall-number bound.
    statement(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET),
    jump(
        BPF_JMP_JGE_K,
        X32_SYSCALL_BIT,
        forward_offset(4, 5),
        forward_offset(4, 6),
    ),
    statement(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
    // Unknown future native syscalls and clone3 fail as unavailable.
    jump(BPF_JMP_JGT_K, MAX_KNOWN_SYSCALL, forward_offset(6, ENOSYS_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_CLONE3, forward_offset(7, ENOSYS_INDEX), 0),
    // Filesystem topology, namespace entry, and device creation are forbidden.
    jump(BPF_JMP_JEQ_K, NR_MKNOD, forward_offset(8, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_PIVOT_ROOT, forward_offset(9, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_CHROOT, forward_offset(10, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_MOUNT, forward_offset(11, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_UMOUNT2, forward_offset(12, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_MKNODAT, forward_offset(13, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_UNSHARE, forward_offset(14, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_OPEN_BY_HANDLE_AT, forward_offset(15, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_SETNS, forward_offset(16, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_OPEN_TREE, forward_offset(17, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_MOVE_MOUNT, forward_offset(18, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_FSOPEN, forward_offset(19, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_FSCONFIG, forward_offset(20, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_FSMOUNT, forward_offset(21, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_FSPICK, forward_offset(22, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_MOUNT_SETATTR, forward_offset(23, EPERM_INDEX), 0),
    jump(BPF_JMP_JEQ_K, NR_OPEN_TREE_ATTR, forward_offset(24, EPERM_INDEX), 0),
    // clone(2) is permitted only when every flag is in SAFE_CLONE_FLAGS.
    jump(BPF_JMP_JEQ_K, NR_CLONE, forward_offset(25, 27), 0),
    statement(BPF_RET_K, SECCOMP_RET_ALLOW),
    statement(BPF_LD_W_ABS, SECCOMP_DATA_ARG0_HIGH_OFFSET),
    jump(BPF_JMP_JEQ_K, 0, 0, forward_offset(28, EPERM_INDEX)),
    statement(BPF_LD_W_ABS, SECCOMP_DATA_ARG0_LOW_OFFSET),
    statement(BPF_ALU_AND_K, UNSAFE_CLONE_FLAGS),
    jump(
        BPF_JMP_JEQ_K,
        0,
        forward_offset(31, ALLOW_INDEX),
        forward_offset(31, EPERM_INDEX),
    ),
    statement(BPF_RET_K, ACTION_EPERM),
    statement(BPF_RET_K, ACTION_ENOSYS),
    statement(BPF_RET_K, SECCOMP_RET_ALLOW),
];

/// Install the fail-closed payload syscall policy for the entire thread group.
///
/// The operation is intentionally irreversible. It must run in the final
/// payload child, after container setup and immediately before invoking
/// untrusted build instructions.
pub(crate) fn install_payload_filter() -> io::Result<()> {
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the payload seccomp policy supports only Linux x86_64",
        ));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    install_x86_64_payload_filter()
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn install_x86_64_payload_filter() -> io::Result<()> {
    // SAFETY: prctl's variadic arguments are passed with the kernel's unsigned
    // long width, and PR_SET_NO_NEW_PRIVS neither dereferences pointers nor
    // retains any argument after returning.
    let set_result = unsafe {
        libc::prctl(
            libc::PR_SET_NO_NEW_PRIVS,
            1 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if set_result == -1 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: PR_GET_NO_NEW_PRIVS ignores the four zero-valued unsigned-long
    // arguments and does not dereference or retain anything.
    let no_new_privs = unsafe {
        libc::prctl(
            libc::PR_GET_NO_NEW_PRIVS,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if no_new_privs == -1 {
        return Err(io::Error::last_os_error());
    }
    if no_new_privs != 1 {
        return Err(io::Error::other(format!(
            "PR_SET_NO_NEW_PRIVS succeeded but PR_GET_NO_NEW_PRIVS returned {no_new_privs}"
        )));
    }

    let mut filter = PAYLOAD_FILTER;
    let filter_len = u16::try_from(filter.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "payload seccomp filter exceeds the kernel instruction limit",
        )
    })?;
    let program = libc::sock_fprog {
        len: filter_len,
        filter: filter.as_mut_ptr(),
    };

    // SAFETY: `program` and `filter` remain live and unmoved for the complete
    // syscall. The kernel copies the classic-BPF program synchronously and
    // retains no userspace pointer. TSYNC applies the copied policy to every
    // thread in the current thread group.
    let seccomp_result = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            SECCOMP_FILTER_FLAG_TSYNC,
            std::ptr::from_ref(&program),
        )
    };
    if seccomp_result == -1 {
        return Err(io::Error::last_os_error());
    }
    if seccomp_result != 0 {
        return Err(io::Error::other(format!(
            "seccomp TSYNC failed to synchronize thread {seccomp_result}"
        )));
    }

    // SAFETY: PR_GET_SECCOMP ignores the four zero-valued unsigned-long
    // arguments and returns the current task's mode directly.
    let mode = unsafe {
        libc::prctl(
            libc::PR_GET_SECCOMP,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if mode == -1 {
        return Err(io::Error::last_os_error());
    }
    if mode != SECCOMP_MODE_FILTER {
        return Err(io::Error::other(format!(
            "seccomp installation returned success but PR_GET_SECCOMP reported mode {mode}"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DENIED_SYSCALLS: [u32; 17] = [
        NR_MKNOD,
        NR_PIVOT_ROOT,
        NR_CHROOT,
        NR_MOUNT,
        NR_UMOUNT2,
        NR_MKNODAT,
        NR_UNSHARE,
        NR_OPEN_BY_HANDLE_AT,
        NR_SETNS,
        NR_OPEN_TREE,
        NR_MOVE_MOUNT,
        NR_FSOPEN,
        NR_FSCONFIG,
        NR_FSMOUNT,
        NR_FSPICK,
        NR_MOUNT_SETATTR,
        NR_OPEN_TREE_ATTR,
    ];

    #[derive(Clone, Copy)]
    struct TestSeccompData {
        nr: u32,
        arch: u32,
        args: [u64; 6],
    }

    impl TestSeccompData {
        fn native(nr: u32) -> Self {
            Self {
                nr,
                arch: AUDIT_ARCH_X86_64,
                args: [0; 6],
            }
        }

        fn clone_with_flags(flags: u64) -> Self {
            let mut input = Self::native(NR_CLONE);
            input.args[0] = flags;
            input
        }

        fn load_word(self, offset: u32) -> u32 {
            match offset {
                SECCOMP_DATA_NR_OFFSET => self.nr,
                SECCOMP_DATA_ARCH_OFFSET => self.arch,
                SECCOMP_DATA_ARG0_LOW_OFFSET => self.args[0] as u32,
                SECCOMP_DATA_ARG0_HIGH_OFFSET => (self.args[0] >> 32) as u32,
                _ => panic!("test interpreter cannot load seccomp_data offset {offset}"),
            }
        }
    }

    /// Minimal classic-BPF interpreter covering exactly the instructions used
    /// by PAYLOAD_FILTER. It lets the tests validate policy behavior without
    /// irreversibly filtering the Rust test harness itself.
    fn interpret(input: TestSeccompData) -> u32 {
        let mut accumulator = 0_u32;
        let mut pc = 0_usize;

        for _ in 0..=PAYLOAD_FILTER.len() {
            let instruction = PAYLOAD_FILTER
                .get(pc)
                .unwrap_or_else(|| panic!("classic-BPF program counter {pc} is out of bounds"));
            match instruction.code {
                BPF_LD_W_ABS => {
                    accumulator = input.load_word(instruction.k);
                    pc += 1;
                }
                BPF_ALU_AND_K => {
                    accumulator &= instruction.k;
                    pc += 1;
                }
                BPF_JMP_JEQ_K | BPF_JMP_JGT_K | BPF_JMP_JGE_K => {
                    let condition = match instruction.code {
                        BPF_JMP_JEQ_K => accumulator == instruction.k,
                        BPF_JMP_JGT_K => accumulator > instruction.k,
                        BPF_JMP_JGE_K => accumulator >= instruction.k,
                        _ => unreachable!(),
                    };
                    let offset = if condition { instruction.jt } else { instruction.jf };
                    pc += usize::from(offset) + 1;
                }
                BPF_RET_K => return instruction.k,
                code => panic!("unsupported classic-BPF instruction {code:#x} at {pc}"),
            }
        }

        panic!("classic-BPF program did not terminate");
    }

    #[test]
    fn every_jump_is_forward_in_bounds_and_every_instruction_is_reachable() {
        let mut reachable = vec![false; PAYLOAD_FILTER.len()];
        let mut pending = vec![0_usize];

        while let Some(pc) = pending.pop() {
            assert!(pc < PAYLOAD_FILTER.len(), "jump target {pc} is out of bounds");
            if std::mem::replace(&mut reachable[pc], true) {
                continue;
            }

            let instruction = PAYLOAD_FILTER[pc];
            match instruction.code {
                BPF_JMP_JEQ_K | BPF_JMP_JGT_K | BPF_JMP_JGE_K => {
                    for offset in [instruction.jt, instruction.jf] {
                        let target = pc + usize::from(offset) + 1;
                        assert!(target > pc, "jump at {pc} is not forward");
                        assert!(
                            target < PAYLOAD_FILTER.len(),
                            "jump at {pc} targets out-of-bounds instruction {target}"
                        );
                        pending.push(target);
                    }
                }
                BPF_RET_K => {}
                BPF_LD_W_ABS | BPF_ALU_AND_K => {
                    let target = pc + 1;
                    assert!(
                        target < PAYLOAD_FILTER.len(),
                        "non-return instruction {pc} falls off the filter"
                    );
                    pending.push(target);
                }
                code => panic!("unsupported classic-BPF instruction {code:#x} at {pc}"),
            }
        }

        assert!(
            reachable.into_iter().all(|instruction| instruction),
            "the filter contains unreachable instructions"
        );
    }

    #[test]
    fn foreign_architectures_are_killed_before_syscall_dispatch() {
        for arch in [0, 0x4000_0003, AUDIT_ARCH_X86_64 ^ 1, u32::MAX] {
            let input = TestSeccompData {
                arch,
                ..TestSeccompData::native(NR_CLONE3)
            };
            assert_eq!(interpret(input), SECCOMP_RET_KILL_PROCESS, "arch {arch:#x}");
        }

        assert_eq!(interpret(TestSeccompData::native(0)), SECCOMP_RET_ALLOW);
    }

    #[test]
    fn x32_and_negative_syscall_numbers_are_killed() {
        for nr in [X32_SYSCALL_BIT, X32_SYSCALL_BIT + NR_CLONE, u32::MAX] {
            assert_eq!(
                interpret(TestSeccompData::native(nr)),
                SECCOMP_RET_KILL_PROCESS,
                "syscall {nr:#x}"
            );
        }
    }

    #[test]
    fn future_native_syscalls_fail_as_unavailable() {
        assert_eq!(interpret(TestSeccompData::native(MAX_KNOWN_SYSCALL)), SECCOMP_RET_ALLOW);
        for nr in [MAX_KNOWN_SYSCALL + 1, 512, 65_535, X32_SYSCALL_BIT - 1] {
            assert_eq!(interpret(TestSeccompData::native(nr)), ACTION_ENOSYS, "syscall {nr}");
        }
        assert_eq!(interpret(TestSeccompData::native(NR_CLONE3)), ACTION_ENOSYS);
    }

    #[test]
    fn audited_native_syscall_table_has_exact_actions() {
        for nr in 0..=MAX_KNOWN_SYSCALL {
            let expected = if nr == NR_CLONE3 {
                ACTION_ENOSYS
            } else if DENIED_SYSCALLS.contains(&nr) {
                ACTION_EPERM
            } else {
                SECCOMP_RET_ALLOW
            };
            assert_eq!(
                interpret(TestSeccompData::native(nr)),
                expected,
                "unexpected action for native syscall {nr}"
            );
        }
    }

    #[test]
    fn every_forbidden_namespace_mount_handle_and_device_syscall_is_eperm() {
        for nr in DENIED_SYSCALLS {
            assert_eq!(interpret(TestSeccompData::native(nr)), ACTION_EPERM, "syscall {nr}");
        }
    }

    #[test]
    fn legacy_clone_checks_every_low_and_high_flag_bit() {
        assert_eq!(interpret(TestSeccompData::clone_with_flags(0)), SECCOMP_RET_ALLOW);
        assert_eq!(
            interpret(TestSeccompData::clone_with_flags(u64::from(SAFE_CLONE_FLAGS))),
            SECCOMP_RET_ALLOW
        );

        for bit in 0..32 {
            let flag = 1_u32 << bit;
            let expected = if flag & SAFE_CLONE_FLAGS == flag {
                SECCOMP_RET_ALLOW
            } else {
                ACTION_EPERM
            };
            assert_eq!(
                interpret(TestSeccompData::clone_with_flags(u64::from(flag))),
                expected,
                "low clone flag bit {bit}"
            );

            let safe_plus_flag = SAFE_CLONE_FLAGS | flag;
            let combined_expected = if flag & SAFE_CLONE_FLAGS == flag {
                SECCOMP_RET_ALLOW
            } else {
                ACTION_EPERM
            };
            assert_eq!(
                interpret(TestSeccompData::clone_with_flags(u64::from(safe_plus_flag))),
                combined_expected,
                "safe mask plus low clone flag bit {bit}"
            );

            let high_flag = 1_u64 << (bit + 32);
            assert_eq!(
                interpret(TestSeccompData::clone_with_flags(high_flag)),
                ACTION_EPERM,
                "high clone flag bit {bit}"
            );
        }

        // Exhaust the entire low 16-bit space as an additional check that the
        // filter implements a mask, rather than accidentally accepting one
        // particular safe combination.
        for flags in 0..=u64::from(u16::MAX) {
            let expected = if flags as u32 & UNSAFE_CLONE_FLAGS == 0 {
                SECCOMP_RET_ALLOW
            } else {
                ACTION_EPERM
            };
            assert_eq!(
                interpret(TestSeccompData::clone_with_flags(flags)),
                expected,
                "low clone flags {flags:#x}"
            );
        }
    }
}
