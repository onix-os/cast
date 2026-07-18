use std::io::{self, Cursor, Read};

use super::super::{
    MAX_INTERRUPTED_SYSCALL_RETRIES,
    mountinfo::{
        MOUNTINFO_LIMITS, MountInfoLimits, parse_mountinfo_with_limits, parse_mountinfo_with_limits_and_work,
        read_mountinfo_bounded, read_mountinfo_with_limits,
    },
};

const RECORD: &[u8] = b"1 1 0:1 / / rw - rootfs rootfs rw\n";

fn limits() -> MountInfoLimits {
    MountInfoLimits {
        max_bytes: 1024,
        max_lines: 16,
        max_fields_per_line: 32,
        max_total_fields: 256,
        max_field_bytes: 128,
        max_work: 8192,
    }
}

#[test]
fn byte_ceiling_admits_n_and_rejects_n_plus_one_for_slices_and_readers() {
    let exact = MountInfoLimits {
        max_bytes: RECORD.len(),
        ..limits()
    };
    assert_eq!(parse_mountinfo_with_limits(RECORD, exact).unwrap().entries().len(), 1);
    assert_eq!(
        read_mountinfo_with_limits(&mut Cursor::new(RECORD), exact)
            .unwrap()
            .entries()
            .len(),
        1
    );

    let mut oversized = RECORD.to_vec();
    oversized.push(b'x');
    assert_eq!(
        parse_mountinfo_with_limits(&oversized, exact).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        read_mountinfo_with_limits(&mut Cursor::new(oversized), exact)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn line_ceiling_admits_n_and_rejects_n_plus_one() {
    let two = b"1 1 0:1 / /one rw - rootfs rootfs rw\n2 1 0:2 / /two rw - tmpfs none rw\n";
    let exact = MountInfoLimits {
        max_lines: 2,
        ..limits()
    };
    assert_eq!(parse_mountinfo_with_limits(two, exact).unwrap().entries().len(), 2);
    assert_eq!(
        parse_mountinfo_with_limits(
            b"1 1 0:1 / /one rw - rootfs rootfs rw\n2 1 0:2 / /two rw - tmpfs none rw\n3 1 0:3 / /three rw - tmpfs none rw\n",
            exact,
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn field_and_option_item_ceiling_admits_n_and_rejects_n_plus_one() {
    let exact = MountInfoLimits {
        // Ten whitespace fields plus two mount options and one super option.
        max_fields_per_line: 13,
        ..limits()
    };
    assert!(parse_mountinfo_with_limits(b"1 1 0:1 / / rw,nodev - rootfs rootfs rw\n", exact).is_ok());
    assert_eq!(
        parse_mountinfo_with_limits(b"1 1 0:1 / / rw,nodev,noexec - rootfs rootfs rw\n", exact)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn total_field_ceiling_rejects_before_copying_the_first_over_budget_option_item() {
    let two = b"1 1 0:1 / /one rw - rootfs rootfs rw\n2 1 0:2 / /two rw - tmpfs none rw\n";
    let exact = MountInfoLimits {
        // Each minimal line consumes ten whitespace fields and two option items.
        max_total_fields: 24,
        ..limits()
    };
    assert!(parse_mountinfo_with_limits(two, exact).is_ok());
    let too_small = MountInfoLimits {
        max_total_fields: 23,
        ..exact
    };
    let error = parse_mountinfo_with_limits(two, too_small).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("option-item limit"));
}

#[test]
fn field_byte_ceiling_admits_n_and_rejects_n_plus_one() {
    let exact = MountInfoLimits {
        max_field_bytes: b"rootfs".len(),
        ..limits()
    };
    assert!(parse_mountinfo_with_limits(RECORD, exact).is_ok());
    let too_small = MountInfoLimits {
        max_field_bytes: b"rootfs".len() - 1,
        ..exact
    };
    assert_eq!(
        parse_mountinfo_with_limits(RECORD, too_small).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn work_ceiling_admits_full_parse_n_and_rejects_n_minus_one() {
    let measuring_limits = MountInfoLimits {
        max_work: usize::MAX,
        ..limits()
    };
    let (parsed, consumed_work) = parse_mountinfo_with_limits_and_work(RECORD, measuring_limits).unwrap();
    assert_eq!(parsed.entries().len(), 1);
    assert!(consumed_work > RECORD.len());

    let exact = MountInfoLimits {
        max_work: consumed_work,
        ..limits()
    };
    let (_, exact_work) = parse_mountinfo_with_limits_and_work(RECORD, exact).unwrap();
    assert_eq!(exact_work, consumed_work);

    let one_too_little = MountInfoLimits {
        max_work: consumed_work - 1,
        ..exact
    };
    assert_eq!(
        parse_mountinfo_with_limits(RECORD, one_too_little).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

struct InterruptForever {
    calls: usize,
}

impl Read for InterruptForever {
    fn read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        Err(io::Error::from(io::ErrorKind::Interrupted))
    }
}

#[test]
fn bounded_reader_never_retries_eintr_without_limit() {
    let mut reader = InterruptForever { calls: 0 };
    assert_eq!(
        read_mountinfo_with_limits(&mut reader, limits()).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(reader.calls, MAX_INTERRUPTED_SYSCALL_RETRIES + 1);
}

struct EndlessBytes {
    calls: usize,
}

impl Read for EndlessBytes {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        output.fill(b'x');
        Ok(output.len())
    }
}

#[test]
fn bounded_reader_uses_one_truncation_sentinel_byte() {
    let exact = MountInfoLimits {
        max_bytes: 7,
        ..limits()
    };
    let mut reader = EndlessBytes { calls: 0 };
    assert_eq!(
        read_mountinfo_with_limits(&mut reader, exact).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(reader.calls, 1);
}

#[test]
fn production_limits_are_finite_and_internally_consistent() {
    assert!(MOUNTINFO_LIMITS.max_bytes < usize::MAX);
    assert!(MOUNTINFO_LIMITS.max_lines > 0);
    assert!(MOUNTINFO_LIMITS.max_fields_per_line >= 12);
    assert!(MOUNTINFO_LIMITS.max_total_fields >= MOUNTINFO_LIMITS.max_fields_per_line);
    assert!(MOUNTINFO_LIMITS.max_field_bytes <= MOUNTINFO_LIMITS.max_bytes);
    assert!(MOUNTINFO_LIMITS.max_work >= MOUNTINFO_LIMITS.max_bytes);
    assert_eq!(
        read_mountinfo_bounded(&mut Cursor::new(RECORD))
            .unwrap()
            .entries()
            .len(),
        1
    );
}
