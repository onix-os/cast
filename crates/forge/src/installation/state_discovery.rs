/// Make one bounded, descriptor-rooted discovery observation of
/// `/usr/.stateID` without following either final component.
///
/// This value is only a stale-client witness. The Client authority gate later
/// performs the complete retained proof. Unsafe, malformed, changing, legacy
/// symlink, FIFO, device, and oversized entries are therefore observations of
/// no usable selection, never pathname fallbacks.
fn read_state_id(root: &std::fs::File) -> Option<state::Id> {
    const MAX_STATE_ID_BYTES: u64 = 10;
    const MAX_READ_ATTEMPTS: usize = 32;
    const STATE_ID_MODE: u32 = 0o644;

    let usr = openat2_file(
        root.as_raw_fd(),
        c"usr",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )
    .ok()?;
    let state_id = openat2_file(
        usr.as_raw_fd(),
        c".stateID",
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .ok()?;
    let before = state_id.metadata().ok()?;
    let mode = before.permissions().mode() & 0o7777;
    if !before.file_type().is_file()
        || before.uid() != Uid::effective().as_raw()
        || mode != STATE_ID_MODE
        || before.nlink() != 1
        || !(1..=MAX_STATE_ID_BYTES).contains(&before.len())
    {
        return None;
    }
    let expected = (
        before.dev(),
        before.ino(),
        before.uid(),
        mode,
        before.nlink(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    );
    let readable = open_path_descriptor_readonly(&state_id).ok()?;
    let expected_length = usize::try_from(before.len()).ok()?;
    let mut bytes = vec![0_u8; expected_length + 1];
    let mut filled = 0usize;
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        if attempts > MAX_READ_ATTEMPTS {
            return None;
        }
        match readable.read_at(&mut bytes[filled..], filled as u64) {
            Ok(0) => break,
            Ok(read) => {
                filled += read;
                if filled == bytes.len() {
                    return None;
                }
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    if filled != expected_length {
        return None;
    }
    bytes.truncate(filled);
    let after = state_id.metadata().ok()?;
    let observed = (
        after.dev(),
        after.ino(),
        after.uid(),
        after.permissions().mode() & 0o7777,
        after.nlink(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    );
    if observed != expected || bytes.len() as u64 != before.len() {
        return None;
    }

    let canonical = bytes.first().is_some_and(|first| *first != b'0') && bytes.iter().all(u8::is_ascii_digit);
    canonical
        .then(|| std::str::from_utf8(&bytes).ok()?.parse::<i32>().ok())
        .flatten()
        .filter(|id| *id > 0)
        .map(state::Id::from)
}
