use std::{
    cell::{Cell, RefCell},
    io,
    os::{fd::OwnedFd, unix::net::UnixStream},
    rc::Rc,
    time::{Duration, Instant},
};

use super::{support::raw_chunk, *};

fn inert_owned_descriptor() -> OwnedFd {
    let (retained, peer) = UnixStream::pair().unwrap();
    drop(peer);
    retained.into()
}

#[test]
fn injected_driver_receives_one_bounded_call_per_source_call() {
    let now = Instant::now();
    let deadline = now + Duration::from_secs(1);
    let chunks = Rc::new(RefCell::new(vec![raw_chunk(&[b"entry"]), Vec::new()].into_iter()));
    let offers = Rc::new(RefCell::new(Vec::new()));
    let call_count = Rc::new(Cell::new(0usize));
    let call = {
        let chunks = Rc::clone(&chunks);
        let offers = Rc::clone(&offers);
        let call_count = Rc::clone(&call_count);
        move |output: &mut [u8]| {
            call_count.set(call_count.get() + 1);
            offers.borrow_mut().push(output.len());
            let chunk = chunks
                .borrow_mut()
                .next()
                .expect("bounded parser made an unexpected call");
            output[..chunk.len()].copy_from_slice(&chunk);
            Ok(chunk.len())
        }
    };

    let (inventory, usage) = ProductionRawDirectoryInventory::read_fresh_directory_with_injected_getdents_until(
        inert_owned_descriptor(),
        ProductionRawDirectoryInventoryLimits::default(),
        deadline,
        move || now,
        call,
    )
    .unwrap();

    assert_eq!(inventory.raw_name(0), Some(b"entry".as_slice()));
    assert_eq!(call_count.get(), usage.read_calls);
    assert_eq!(call_count.get(), 2);
    assert_eq!(offers.borrow().as_slice(), &[32 * 1024, 32 * 1024]);
}

#[test]
fn interrupted_syscall_is_failed_closed_without_retry() {
    let now = Instant::now();
    let calls = Rc::new(Cell::new(0usize));
    let call = {
        let calls = Rc::clone(&calls);
        move |_output: &mut [u8]| {
            calls.set(calls.get() + 1);
            Err(io::Error::from(io::ErrorKind::Interrupted))
        }
    };

    let error = ProductionRawDirectoryInventory::read_fresh_directory_with_injected_getdents_until(
        inert_owned_descriptor(),
        ProductionRawDirectoryInventoryLimits::default(),
        now + Duration::from_secs(1),
        move || now,
        call,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ProductionRawDirectoryInventoryError::SourceFailed { .. }
    ));
    assert_eq!(calls.get(), 1);
}

#[test]
fn deadline_expiry_during_call_is_detected_immediately_after_return() {
    let now = Instant::now();
    let deadline = now + Duration::from_secs(1);
    let clock = Rc::new(Cell::new(now));
    let calls = Rc::new(Cell::new(0usize));
    let call = {
        let clock = Rc::clone(&clock);
        let calls = Rc::clone(&calls);
        move |_output: &mut [u8]| {
            calls.set(calls.get() + 1);
            clock.set(deadline + Duration::from_nanos(1));
            Ok(0)
        }
    };
    let read_clock = Rc::clone(&clock);

    let error = ProductionRawDirectoryInventory::read_fresh_directory_with_injected_getdents_until(
        inert_owned_descriptor(),
        ProductionRawDirectoryInventoryLimits::default(),
        deadline,
        move || read_clock.get(),
        call,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ProductionRawDirectoryInventoryError::DeadlineExceeded { .. }
    ));
    assert_eq!(calls.get(), 1);
}

#[test]
fn native_getdents64_reads_only_an_ordinary_target_fixture() {
    let target = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&target).unwrap();
    let temporary = tempfile::Builder::new()
        .prefix("forge-getdents64-")
        .tempdir_in(&target)
        .unwrap();
    std::fs::write(temporary.path().join("alpha"), []).unwrap();
    std::fs::create_dir(temporary.path().join("nested")).unwrap();
    let directory: OwnedFd = std::fs::File::open(temporary.path()).unwrap().into();

    let (inventory, usage) = ProductionRawDirectoryInventory::read_fresh_linux_directory_with_usage_until(
        directory,
        ProductionRawDirectoryInventoryLimits::default(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    let mut names = (0..inventory.len())
        .map(|index| inventory.raw_name(index).unwrap().to_vec())
        .collect::<Vec<_>>();
    names.sort_unstable();

    assert_eq!(names, vec![b"alpha".to_vec(), b"nested".to_vec()]);
    assert!(usage.read_calls >= 1);
    assert!(usage.read_bytes > 0);

    let empty = tempfile::Builder::new()
        .prefix("forge-getdents64-empty-")
        .tempdir_in(&target)
        .unwrap();
    let empty_directory: OwnedFd = std::fs::File::open(empty.path()).unwrap().into();
    let empty_inventory = ProductionRawDirectoryInventory::read_fresh_linux_directory_until(
        empty_directory,
        ProductionRawDirectoryInventoryLimits::default(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert!(empty_inventory.is_empty());
}
