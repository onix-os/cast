//! Fixed, descriptor-only transport for the privileged private-device broker.

use std::io;
use std::mem::{offset_of, size_of, zeroed};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd, RawFd};
use std::time::{Duration, Instant};

use nix::libc;
use snafu::Snafu;

use super::private_devices::{
    PRIVATE_DEVICE_COUNT, PrivateDeviceError, PrivateDeviceMounts, provision_private_device_mounts,
};

pub(crate) const PRIVATE_DEVICE_BROKER_SOCKET: &str = "/run/cast/private-devices.socket";
pub(crate) const PRIVATE_DEVICE_BROKER_PROTOCOL_VERSION: u16 = 1;

const REQUEST_FRAME: [u8; 16] = *b"CASTPDEV\0\x01\x01\0\0\0\0\0";
const RESPONSE_FRAME: [u8; 16] = *b"CASTPDEV\0\x01\x02\0\0\0\0\0";
const SEND_FLAGS: libc::c_int = libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL;
const RECEIVE_FLAGS: libc::c_int = libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC;

/// Request one fresh private device set from the fixed root-owned broker.
///
/// One monotonic deadline bounds connect, request send, and response receive.
/// The protocol contains no caller-selected path, identity, mode, or count.
pub(crate) fn request_private_device_mounts(timeout: Duration) -> Result<PrivateDeviceMounts, BrokerError> {
    let deadline = Deadline::new(timeout)?;
    let connection = connect_default_broker(&deadline)?;
    require_seqpacket(connection.as_fd())?;
    require_root_peer(connection.as_fd())?;
    send_packet(
        connection.as_fd(),
        &REQUEST_FRAME,
        &[],
        &deadline,
        "send broker request",
    )?;
    receive_response(connection.as_fd(), &deadline)
}

/// Serve exactly one systemd `Accept=yes` connection, then return.
///
/// The supplied descriptor is already connected. The server accepts one exact
/// descriptor-free request, provisions one fresh set, and emits one exact
/// response carrying a single three-FD `SCM_RIGHTS` record.
pub(crate) fn serve_private_device_connection(connection: OwnedFd, timeout: Duration) -> Result<(), BrokerError> {
    let deadline = Deadline::new(timeout)?;
    require_seqpacket(connection.as_fd())?;
    receive_request(connection.as_fd(), &deadline)?;
    let mounts = provision_private_device_mounts().map_err(|source| BrokerError::PrivateDevices { source })?;
    let descriptors = mounts.ordered().map(|(_, descriptor)| descriptor.as_raw_fd());
    send_packet(
        connection.as_fd(),
        &RESPONSE_FRAME,
        &descriptors,
        &deadline,
        "send broker response",
    )
}

#[derive(Debug, Snafu)]
pub(crate) enum BrokerError {
    #[snafu(display("private-device broker timeout is too large for a monotonic deadline"))]
    InvalidTimeout,
    #[snafu(display("{operation}"))]
    Syscall { operation: &'static str, source: io::Error },
    #[snafu(display("timed out while attempting to {operation}"))]
    Timeout { operation: &'static str },
    #[snafu(display("private-device broker socket path is too long"))]
    SocketPathTooLong,
    #[snafu(display("private-device broker connection has socket type {actual}; expected SOCK_SEQPACKET"))]
    UnexpectedSocketType { actual: libc::c_int },
    #[snafu(display("private-device broker peer is {uid}:{gid} pid {pid}; expected uid 0"))]
    UntrustedPeer {
        pid: libc::pid_t,
        uid: libc::uid_t,
        gid: libc::gid_t,
    },
    #[snafu(display("peer closed while attempting to {operation}"))]
    ConnectionClosed { operation: &'static str },
    #[snafu(display("{operation} transferred {actual} payload bytes; expected exactly {expected}"))]
    PartialSend {
        operation: &'static str,
        expected: usize,
        actual: usize,
    },
    #[snafu(display("{context} payload was truncated"))]
    PayloadTruncated { context: &'static str },
    #[snafu(display("{context} control data was truncated"))]
    ControlTruncated { context: &'static str },
    #[snafu(display("{context} has {actual} bytes; expected exactly {expected}"))]
    UnexpectedFrameLength {
        context: &'static str,
        expected: usize,
        actual: usize,
    },
    #[snafu(display("{context} does not match protocol version {version}"))]
    UnexpectedFrame { context: &'static str, version: u16 },
    #[snafu(display(
        "{context} contains {records} control records, {rights_records} SCM_RIGHTS records, and {descriptors} descriptors; expected {expected_descriptors} descriptors in {expected_records} SCM_RIGHTS records"
    ))]
    UnexpectedControlEnvelope {
        context: &'static str,
        records: usize,
        rights_records: usize,
        descriptors: usize,
        expected_records: usize,
        expected_descriptors: usize,
    },
    #[snafu(display("{context} contains malformed ancillary data"))]
    MalformedControl { context: &'static str },
    #[snafu(display("received private-device descriptor {index} is not close-on-exec"))]
    DescriptorNotCloseOnExec { index: usize },
    #[snafu(display("validate broker-provided private devices"))]
    PrivateDevices { source: PrivateDeviceError },
}

impl BrokerError {
    /// Whether the fixed broker execution capability is absent at its narrow
    /// endpoint-admission boundary.
    ///
    /// Peer authentication, framing, ancillary-data, descriptor, and private
    /// device validation failures are never softened, even if an inner error
    /// happens to be permission-shaped.
    pub(crate) fn execution_capability_unavailable(&self) -> bool {
        match self {
            Self::Syscall { operation, source }
                if matches!(
                    *operation,
                    "create private-device broker socket" | "connect to private-device broker"
                ) =>
            {
                matches!(
                    source.raw_os_error(),
                    Some(
                        libc::ENOENT | libc::ECONNREFUSED | libc::EACCES | libc::EPERM | libc::ENOSYS | libc::ETIMEDOUT
                    )
                )
            }
            Self::Timeout { .. } | Self::ConnectionClosed { .. } => true,
            Self::InvalidTimeout
            | Self::Syscall { .. }
            | Self::SocketPathTooLong
            | Self::UnexpectedSocketType { .. }
            | Self::UntrustedPeer { .. }
            | Self::PartialSend { .. }
            | Self::PayloadTruncated { .. }
            | Self::ControlTruncated { .. }
            | Self::UnexpectedFrameLength { .. }
            | Self::UnexpectedFrame { .. }
            | Self::UnexpectedControlEnvelope { .. }
            | Self::MalformedControl { .. }
            | Self::DescriptorNotCloseOnExec { .. }
            | Self::PrivateDevices { .. } => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PeerCredentials {
    pid: libc::pid_t,
    uid: libc::uid_t,
    gid: libc::gid_t,
}

fn require_root_peer(connection: BorrowedFd<'_>) -> Result<(), BrokerError> {
    let credentials = peer_credentials(connection)?;
    validate_root_peer(credentials)
}

fn validate_root_peer(credentials: PeerCredentials) -> Result<(), BrokerError> {
    if credentials.uid == 0 {
        Ok(())
    } else {
        Err(BrokerError::UntrustedPeer {
            pid: credentials.pid,
            uid: credentials.uid,
            gid: credentials.gid,
        })
    }
}

fn peer_credentials(connection: BorrowedFd<'_>) -> Result<PeerCredentials, BrokerError> {
    // SAFETY: zero is a valid output initialization; connection remains live,
    // and length describes the exact writable ucred allocation.
    let mut credentials: libc::ucred = unsafe { zeroed() };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            connection.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut libc::ucred as *mut libc::c_void,
            &mut length,
        )
    };
    if result == -1 {
        return Err(last_error("authenticate broker peer with SO_PEERCRED"));
    }
    require_getsockopt_length(length, size_of::<libc::ucred>(), "SO_PEERCRED response")?;
    Ok(PeerCredentials {
        pid: credentials.pid,
        uid: credentials.uid,
        gid: credentials.gid,
    })
}

fn connect_default_broker(deadline: &Deadline) -> Result<OwnedFd, BrokerError> {
    // SAFETY: socket has no borrowed pointers and returns one fresh descriptor.
    let descriptor = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if descriptor == -1 {
        return Err(last_error("create private-device broker socket"));
    }
    // SAFETY: successful socket returned one fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let (address, length) = default_socket_address()?;

    loop {
        // SAFETY: address and its exact initialized length remain live for the
        // call; descriptor is a nonblocking AF_UNIX socket.
        let result = unsafe {
            libc::connect(
                descriptor.as_raw_fd(),
                &address as *const libc::sockaddr_un as *const libc::sockaddr,
                length,
            )
        };
        if result == 0 {
            return Ok(descriptor);
        }
        let source = io::Error::last_os_error();
        match source.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EINPROGRESS | libc::EALREADY | libc::EAGAIN) => {
                wait_ready(
                    descriptor.as_fd(),
                    libc::POLLOUT,
                    deadline,
                    "connect to private-device broker",
                )?;
                let pending = socket_error(descriptor.as_fd())?;
                if pending == 0 {
                    return Ok(descriptor);
                }
                if matches!(pending, libc::EINPROGRESS | libc::EALREADY | libc::EAGAIN) {
                    continue;
                }
                return Err(BrokerError::Syscall {
                    operation: "connect to private-device broker",
                    source: io::Error::from_raw_os_error(pending),
                });
            }
            _ => {
                return Err(BrokerError::Syscall {
                    operation: "connect to private-device broker",
                    source,
                });
            }
        }
    }
}

fn default_socket_address() -> Result<(libc::sockaddr_un, libc::socklen_t), BrokerError> {
    // SAFETY: zero is valid initialization for sockaddr_un.
    let mut address: libc::sockaddr_un = unsafe { zeroed() };
    let path = PRIVATE_DEVICE_BROKER_SOCKET.as_bytes();
    if path.len() + 1 > address.sun_path.len() {
        return Err(BrokerError::SocketPathTooLong);
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address.sun_path.iter_mut().zip(path.iter().copied()) {
        *target = source as libc::c_char;
    }
    let length = offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1;
    let length = libc::socklen_t::try_from(length).map_err(|_| BrokerError::SocketPathTooLong)?;
    Ok((address, length))
}

fn socket_error(connection: BorrowedFd<'_>) -> Result<libc::c_int, BrokerError> {
    let mut error = 0;
    let mut length = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: error and length are exact writable output allocations.
    if unsafe {
        libc::getsockopt(
            connection.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            &mut error as *mut libc::c_int as *mut libc::c_void,
            &mut length,
        )
    } == -1
    {
        return Err(last_error("read private-device broker connect status"));
    }
    require_getsockopt_length(length, size_of::<libc::c_int>(), "SO_ERROR response")?;
    Ok(error)
}

fn require_seqpacket(connection: BorrowedFd<'_>) -> Result<(), BrokerError> {
    let mut socket_type = 0;
    let mut length = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: socket_type and length are exact writable output allocations.
    if unsafe {
        libc::getsockopt(
            connection.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            &mut socket_type as *mut libc::c_int as *mut libc::c_void,
            &mut length,
        )
    } == -1
    {
        return Err(last_error("verify private-device broker socket type"));
    }
    require_getsockopt_length(length, size_of::<libc::c_int>(), "SO_TYPE response")?;
    if socket_type == libc::SOCK_SEQPACKET {
        Ok(())
    } else {
        Err(BrokerError::UnexpectedSocketType { actual: socket_type })
    }
}

fn require_getsockopt_length(
    actual: libc::socklen_t,
    expected: usize,
    context: &'static str,
) -> Result<(), BrokerError> {
    if actual as usize == expected {
        Ok(())
    } else {
        Err(BrokerError::MalformedControl { context })
    }
}

fn receive_request(connection: BorrowedFd<'_>, deadline: &Deadline) -> Result<(), BrokerError> {
    let mut payload = [0_u8; REQUEST_FRAME.len()];
    let packet = receive_packet(connection, &mut payload, 1, deadline, "receive broker request")?;
    require_untruncated(&packet, "broker request")?;
    require_frame(&payload, packet.bytes, &REQUEST_FRAME, "broker request")?;
    if packet.controls.records != 0 || !packet.controls.descriptors.is_empty() {
        return Err(control_error("broker request", &packet.controls, 0, 0));
    }
    Ok(())
}

fn receive_response(connection: BorrowedFd<'_>, deadline: &Deadline) -> Result<PrivateDeviceMounts, BrokerError> {
    let mut payload = [0_u8; RESPONSE_FRAME.len()];
    let packet = receive_packet(
        connection,
        &mut payload,
        PRIVATE_DEVICE_COUNT,
        deadline,
        "receive broker response",
    )?;
    require_untruncated(&packet, "broker response")?;
    require_frame(&payload, packet.bytes, &RESPONSE_FRAME, "broker response")?;
    let expected_rights_bytes = PRIVATE_DEVICE_COUNT * size_of::<RawFd>();
    if packet.controls.records != 1
        || packet.controls.rights_records != 1
        || packet.controls.descriptors.len() != PRIVATE_DEVICE_COUNT
        || packet.controls.rights_payload_bytes.as_slice() != [expected_rights_bytes]
    {
        return Err(control_error(
            "broker response",
            &packet.controls,
            1,
            PRIVATE_DEVICE_COUNT,
        ));
    }
    for (index, descriptor) in packet.controls.descriptors.iter().enumerate() {
        // SAFETY: F_GETFD is a read-only query of the live received descriptor.
        let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) };
        if flags == -1 {
            return Err(last_error("inspect received private-device descriptor"));
        }
        if flags & libc::FD_CLOEXEC == 0 {
            return Err(BrokerError::DescriptorNotCloseOnExec { index });
        }
    }
    let descriptors: [OwnedFd; PRIVATE_DEVICE_COUNT] = packet
        .controls
        .descriptors
        .try_into()
        .map_err(|descriptors: Vec<OwnedFd>| control_error_count("broker response", descriptors.len(), 1))?;
    PrivateDeviceMounts::from_received(descriptors).map_err(|source| BrokerError::PrivateDevices { source })
}

fn send_packet(
    connection: BorrowedFd<'_>,
    payload: &[u8],
    descriptors: &[RawFd],
    deadline: &Deadline,
    operation: &'static str,
) -> Result<(), BrokerError> {
    let mut payload_iovec = libc::iovec {
        iov_base: payload.as_ptr() as *mut libc::c_void,
        iov_len: payload.len(),
    };
    let mut control = (!descriptors.is_empty()).then(|| ControlBuffer::for_descriptors(descriptors.len()));
    // SAFETY: zero is valid initialization for msghdr.
    let mut message: libc::msghdr = unsafe { zeroed() };
    message.msg_iov = &mut payload_iovec;
    message.msg_iovlen = 1;
    if let Some(control) = control.as_mut() {
        message.msg_control = control.as_mut_ptr();
        message.msg_controllen = control.bytes;
        // SAFETY: the aligned control allocation has CMSG_SPACE for every raw
        // descriptor and message points at it with the exact length.
        let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        if header.is_null() {
            return Err(BrokerError::MalformedControl { context: operation });
        }
        // SAFETY: header and its payload are contained in the live allocation.
        unsafe {
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN((descriptors.len() * size_of::<RawFd>()) as libc::c_uint) as usize;
            std::ptr::copy_nonoverlapping(
                descriptors.as_ptr(),
                libc::CMSG_DATA(header) as *mut RawFd,
                descriptors.len(),
            );
        }
    }

    loop {
        // SAFETY: message borrows the live payload/control allocations only
        // for this nonblocking sendmsg call.
        let sent = unsafe { libc::sendmsg(connection.as_raw_fd(), &message, SEND_FLAGS) };
        if sent >= 0 {
            let sent = usize::try_from(sent).expect("nonnegative ssize_t fits usize");
            return if sent == payload.len() {
                Ok(())
            } else {
                Err(BrokerError::PartialSend {
                    operation,
                    expected: payload.len(),
                    actual: sent,
                })
            };
        }
        let source = io::Error::last_os_error();
        match source.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => wait_ready(connection, libc::POLLOUT, deadline, operation)?,
            _ => return Err(BrokerError::Syscall { operation, source }),
        }
    }
}

fn receive_packet(
    connection: BorrowedFd<'_>,
    payload: &mut [u8],
    descriptor_capacity: usize,
    deadline: &Deadline,
    operation: &'static str,
) -> Result<ReceivedPacket, BrokerError> {
    let mut payload_iovec = libc::iovec {
        iov_base: payload.as_mut_ptr() as *mut libc::c_void,
        iov_len: payload.len(),
    };
    let mut control = ControlBuffer::for_descriptors(descriptor_capacity.max(1));
    loop {
        // Reinitialize kernel-owned output fields on every retry.
        // SAFETY: zero is valid initialization for msghdr.
        let mut message: libc::msghdr = unsafe { zeroed() };
        message.msg_iov = &mut payload_iovec;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr();
        message.msg_controllen = control.bytes;
        // SAFETY: message points to live, bounded output allocations for this
        // nonblocking recvmsg call. MSG_CMSG_CLOEXEC closes the exec race.
        let received = unsafe { libc::recvmsg(connection.as_raw_fd(), &mut message, RECEIVE_FLAGS) };
        if received > 0 {
            let controls = collect_controls(&mut message, operation)?;
            return Ok(ReceivedPacket {
                bytes: usize::try_from(received).expect("positive ssize_t fits usize"),
                flags: message.msg_flags,
                controls,
            });
        }
        if received == 0 {
            return Err(BrokerError::ConnectionClosed { operation });
        }
        let source = io::Error::last_os_error();
        match source.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => wait_ready(connection, libc::POLLIN, deadline, operation)?,
            _ => return Err(BrokerError::Syscall { operation, source }),
        }
    }
}

struct ReceivedPacket {
    bytes: usize,
    flags: libc::c_int,
    controls: ReceivedControls,
}

#[derive(Default)]
struct ReceivedControls {
    records: usize,
    rights_records: usize,
    rights_payload_bytes: Vec<usize>,
    descriptors: Vec<OwnedFd>,
}

fn collect_controls(message: &mut libc::msghdr, context: &'static str) -> Result<ReceivedControls, BrokerError> {
    let mut result = ReceivedControls::default();
    // SAFETY: recvmsg initialized the bounded control allocation described by
    // message; the CMSG traversal macros stay within msg_controllen.
    let mut current = unsafe { libc::CMSG_FIRSTHDR(message) };
    while !current.is_null() {
        result.records += 1;
        // SAFETY: current came from the kernel-validated CMSG traversal.
        let header = unsafe { &*current };
        // SAFETY: zero requests only the fixed cmsghdr prefix size.
        let header_bytes = unsafe { libc::CMSG_LEN(0) } as usize;
        if header.cmsg_len < header_bytes {
            return Err(BrokerError::MalformedControl { context });
        }
        if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_RIGHTS {
            result.rights_records += 1;
            let payload_bytes = header.cmsg_len - header_bytes;
            if payload_bytes % size_of::<RawFd>() != 0 {
                return Err(BrokerError::MalformedControl { context });
            }
            result.rights_payload_bytes.push(payload_bytes);
            let count = payload_bytes / size_of::<RawFd>();
            for index in 0..count {
                // SAFETY: the kernel supplied payload contains count raw FDs;
                // read_unaligned avoids assumptions about cmsghdr padding.
                let descriptor =
                    unsafe { std::ptr::read_unaligned((libc::CMSG_DATA(current) as *const RawFd).add(index)) };
                if descriptor < 0 {
                    return Err(BrokerError::MalformedControl { context });
                }
                // SAFETY: SCM_RIGHTS installed one fresh descriptor owned by
                // this process; transfer it immediately to RAII cleanup.
                result.descriptors.push(unsafe { OwnedFd::from_raw_fd(descriptor) });
            }
        }
        // SAFETY: current belongs to message's live bounded control buffer.
        current = unsafe { libc::CMSG_NXTHDR(message, current) };
    }
    Ok(result)
}

struct ControlBuffer {
    words: Vec<usize>,
    bytes: usize,
}

impl ControlBuffer {
    fn for_descriptors(count: usize) -> Self {
        let payload = count * size_of::<RawFd>();
        // SAFETY: payload is a small bounded descriptor array size.
        let bytes = unsafe { libc::CMSG_SPACE(payload as libc::c_uint) } as usize;
        let words = bytes.div_ceil(size_of::<usize>());
        Self {
            words: vec![0; words],
            bytes,
        }
    }

    fn as_mut_ptr(&mut self) -> *mut libc::c_void {
        self.words.as_mut_ptr() as *mut libc::c_void
    }
}

fn require_untruncated(packet: &ReceivedPacket, context: &'static str) -> Result<(), BrokerError> {
    if packet.flags & libc::MSG_TRUNC != 0 {
        Err(BrokerError::PayloadTruncated { context })
    } else if packet.flags & libc::MSG_CTRUNC != 0 {
        Err(BrokerError::ControlTruncated { context })
    } else {
        Ok(())
    }
}

fn require_frame(payload: &[u8], received: usize, expected: &[u8], context: &'static str) -> Result<(), BrokerError> {
    if received != expected.len() {
        return Err(BrokerError::UnexpectedFrameLength {
            context,
            expected: expected.len(),
            actual: received,
        });
    }
    if &payload[..received] == expected {
        Ok(())
    } else {
        Err(BrokerError::UnexpectedFrame {
            context,
            version: PRIVATE_DEVICE_BROKER_PROTOCOL_VERSION,
        })
    }
}

fn control_error(
    context: &'static str,
    controls: &ReceivedControls,
    expected_records: usize,
    expected_descriptors: usize,
) -> BrokerError {
    BrokerError::UnexpectedControlEnvelope {
        context,
        records: controls.records,
        rights_records: controls.rights_records,
        descriptors: controls.descriptors.len(),
        expected_records,
        expected_descriptors,
    }
}

fn control_error_count(context: &'static str, descriptors: usize, expected_records: usize) -> BrokerError {
    BrokerError::UnexpectedControlEnvelope {
        context,
        records: expected_records,
        rights_records: expected_records,
        descriptors,
        expected_records,
        expected_descriptors: PRIVATE_DEVICE_COUNT,
    }
}

struct Deadline {
    expires: Instant,
}

impl Deadline {
    fn new(timeout: Duration) -> Result<Self, BrokerError> {
        let expires = Instant::now().checked_add(timeout).ok_or(BrokerError::InvalidTimeout)?;
        Ok(Self { expires })
    }

    fn poll_milliseconds(&self, operation: &'static str) -> Result<libc::c_int, BrokerError> {
        let now = Instant::now();
        if now >= self.expires {
            return Err(BrokerError::Timeout { operation });
        }
        let remaining = self.expires.duration_since(now);
        let rounded_up = remaining.as_millis() + u128::from(remaining.subsec_nanos() % 1_000_000 != 0);
        Ok(rounded_up.min(libc::c_int::MAX as u128) as libc::c_int)
    }
}

fn wait_ready(
    descriptor: BorrowedFd<'_>,
    events: libc::c_short,
    deadline: &Deadline,
    operation: &'static str,
) -> Result<(), BrokerError> {
    loop {
        let mut poll = libc::pollfd {
            fd: descriptor.as_raw_fd(),
            events,
            revents: 0,
        };
        let timeout = deadline.poll_milliseconds(operation)?;
        // SAFETY: poll points at exactly one initialized pollfd.
        let result = unsafe { libc::poll(&mut poll, 1, timeout) };
        if result > 0 {
            if poll.revents & libc::POLLNVAL != 0 {
                return Err(BrokerError::Syscall {
                    operation,
                    source: io::Error::from_raw_os_error(libc::EBADF),
                });
            }
            return Ok(());
        }
        if result == 0 {
            return Err(BrokerError::Timeout { operation });
        }
        let source = io::Error::last_os_error();
        if source.raw_os_error() != Some(libc::EINTR) {
            return Err(BrokerError::Syscall { operation, source });
        }
    }
}

fn last_error(operation: &'static str) -> BrokerError {
    BrokerError::Syscall {
        operation,
        source: io::Error::last_os_error(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (OwnedFd, OwnedFd) {
        let mut descriptors = [-1; 2];
        // SAFETY: descriptors is an exact two-element output array.
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                    0,
                    descriptors.as_mut_ptr(),
                )
            },
            0
        );
        // SAFETY: successful socketpair returned two distinct fresh FDs.
        unsafe {
            (
                OwnedFd::from_raw_fd(descriptors[0]),
                OwnedFd::from_raw_fd(descriptors[1]),
            )
        }
    }

    fn deadline() -> Deadline {
        Deadline::new(Duration::from_secs(1)).unwrap()
    }

    #[test]
    fn protocol_is_fixed_and_credential_policy_is_pure() {
        assert_eq!(PRIVATE_DEVICE_BROKER_SOCKET, "/run/cast/private-devices.socket");
        assert_eq!(PRIVATE_DEVICE_BROKER_PROTOCOL_VERSION, 1);
        assert_eq!(REQUEST_FRAME.len(), 16);
        assert_eq!(RESPONSE_FRAME.len(), 16);
        assert_ne!(REQUEST_FRAME, RESPONSE_FRAME);
        assert!(validate_root_peer(PeerCredentials { pid: 7, uid: 0, gid: 0 }).is_ok());
        assert!(matches!(
            validate_root_peer(PeerCredentials {
                pid: 7,
                uid: 1_000,
                gid: 1_000,
            }),
            Err(BrokerError::UntrustedPeer { uid: 1_000, .. })
        ));

    }

    #[test]
    fn getsockopt_outputs_require_exact_lengths() {
        for (expected, context) in [
            (size_of::<libc::ucred>(), "SO_PEERCRED response"),
            (size_of::<libc::c_int>(), "SO_ERROR response"),
            (size_of::<libc::c_int>(), "SO_TYPE response"),
        ] {
            assert!(require_getsockopt_length(expected as libc::socklen_t, expected, context).is_ok());
            for actual in [0, expected - 1, expected + 1] {
                assert!(matches!(
                    require_getsockopt_length(actual as libc::socklen_t, expected, context),
                    Err(BrokerError::MalformedControl { context: rejected }) if rejected == context
                ));
            }
        }
    }

    #[test]
    fn capability_classifier_accepts_only_fixed_endpoint_admission_failures() {
        for code in [
            libc::ENOENT,
            libc::ECONNREFUSED,
            libc::EACCES,
            libc::EPERM,
            libc::ENOSYS,
            libc::ETIMEDOUT,
        ] {
            assert!(
                BrokerError::Syscall {
                    operation: "connect to private-device broker",
                    source: io::Error::from_raw_os_error(code),
                }
                .execution_capability_unavailable(),
                "rejected errno {code}"
            );
        }
        assert!(BrokerError::Timeout { operation: "connect" }.execution_capability_unavailable());
        assert!(BrokerError::ConnectionClosed { operation: "receive" }.execution_capability_unavailable());
    }

    #[test]
    fn capability_classifier_keeps_protocol_and_nested_failures_hard() {
        assert!(
            !BrokerError::Syscall {
                operation: "send broker request",
                source: io::Error::from_raw_os_error(libc::EPERM),
            }
            .execution_capability_unavailable()
        );
        assert!(
            !BrokerError::Syscall {
                operation: "connect to private-device broker",
                source: io::Error::from_raw_os_error(libc::EIO),
            }
            .execution_capability_unavailable()
        );
        assert!(
            !BrokerError::UntrustedPeer {
                pid: 1,
                uid: 1_000,
                gid: 1_000,
            }
            .execution_capability_unavailable()
        );
        assert!(
            !BrokerError::UnexpectedFrame {
                context: "test",
                version: PRIVATE_DEVICE_BROKER_PROTOCOL_VERSION,
            }
            .execution_capability_unavailable()
        );
        assert!(
            !BrokerError::UnexpectedControlEnvelope {
                context: "test",
                records: 0,
                rights_records: 0,
                descriptors: 0,
                expected_records: 1,
                expected_descriptors: PRIVATE_DEVICE_COUNT,
            }
            .execution_capability_unavailable()
        );
        assert!(!BrokerError::DescriptorNotCloseOnExec { index: 0 }.execution_capability_unavailable());
    }

    #[test]
    fn request_requires_one_exact_descriptor_free_packet() {
        let (sender, receiver) = pair();
        send_packet(sender.as_fd(), &REQUEST_FRAME, &[], &deadline(), "test send").unwrap();
        receive_request(receiver.as_fd(), &deadline()).unwrap();

        let (sender, receiver) = pair();
        let mut wrong = REQUEST_FRAME;
        wrong[8] ^= 1;
        send_packet(sender.as_fd(), &wrong, &[], &deadline(), "test send").unwrap();
        assert!(matches!(
            receive_request(receiver.as_fd(), &deadline()),
            Err(BrokerError::UnexpectedFrame { .. })
        ));

        let (sender, receiver) = pair();
        send_packet(
            sender.as_fd(),
            &REQUEST_FRAME[..REQUEST_FRAME.len() - 1],
            &[],
            &deadline(),
            "test send",
        )
        .unwrap();
        assert!(matches!(
            receive_request(receiver.as_fd(), &deadline()),
            Err(BrokerError::UnexpectedFrameLength {
                expected: 16,
                actual: 15,
                ..
            })
        ));

        let (sender, receiver) = pair();
        let passed = [sender.as_raw_fd()];
        send_packet(sender.as_fd(), &REQUEST_FRAME, &passed, &deadline(), "test send").unwrap();
        assert!(matches!(
            receive_request(receiver.as_fd(), &deadline()),
            Err(BrokerError::UnexpectedControlEnvelope { descriptors: 1, .. })
        ));
    }

    #[test]
    fn response_rejects_missing_extra_and_truncated_descriptor_sets() {
        let (sender, receiver) = pair();
        send_packet(sender.as_fd(), &RESPONSE_FRAME, &[], &deadline(), "test send").unwrap();
        assert!(matches!(
            receive_response(receiver.as_fd(), &deadline()),
            Err(BrokerError::UnexpectedControlEnvelope { descriptors: 0, .. })
        ));

        let (sender, receiver) = pair();
        let extras = [sender.as_raw_fd(); PRIVATE_DEVICE_COUNT + 1];
        send_packet(sender.as_fd(), &RESPONSE_FRAME, &extras, &deadline(), "test send").unwrap();
        assert!(matches!(
            receive_response(receiver.as_fd(), &deadline()),
            Err(BrokerError::UnexpectedControlEnvelope { descriptors: 4, .. })
        ));

        let (sender, receiver) = pair();
        // Four raw FDs can occupy the same aligned CMSG_SPACE as three on a
        // 64-bit ABI. Five necessarily exceeds the exact receive allocation.
        let truncated = [sender.as_raw_fd(); PRIVATE_DEVICE_COUNT + 2];
        send_packet(sender.as_fd(), &RESPONSE_FRAME, &truncated, &deadline(), "test send").unwrap();
        assert!(matches!(
            receive_response(receiver.as_fd(), &deadline()),
            Err(BrokerError::ControlTruncated { .. })
        ));
    }

    #[test]
    fn oversized_payload_is_rejected_without_accepting_its_descriptors() {
        let (sender, receiver) = pair();
        let mut oversized = [0_u8; RESPONSE_FRAME.len() + 1];
        oversized[..RESPONSE_FRAME.len()].copy_from_slice(&RESPONSE_FRAME);
        let descriptors = [sender.as_raw_fd(); PRIVATE_DEVICE_COUNT];
        send_packet(sender.as_fd(), &oversized, &descriptors, &deadline(), "test send").unwrap();
        assert!(matches!(
            receive_response(receiver.as_fd(), &deadline()),
            Err(BrokerError::PayloadTruncated { .. })
        ));
    }

    #[test]
    fn receive_timeout_is_explicit_and_monotonic() {
        let (_sender, receiver) = pair();
        let expired = Deadline::new(Duration::ZERO).unwrap();
        assert!(matches!(
            receive_request(receiver.as_fd(), &expired),
            Err(BrokerError::Timeout {
                operation: "receive broker request"
            })
        ));
    }

    #[test]
    fn validation_failure_closes_every_received_descriptor() {
        let (sender, receiver) = pair();
        let (probe, passed) = pair();
        let descriptors = [passed.as_raw_fd(); PRIVATE_DEVICE_COUNT];
        send_packet(sender.as_fd(), &RESPONSE_FRAME, &descriptors, &deadline(), "test send").unwrap();
        drop(passed);
        let error = receive_response(receiver.as_fd(), &deadline()).unwrap_err();
        assert!(matches!(&error, BrokerError::PrivateDevices { .. }));
        assert!(!error.execution_capability_unavailable());

        let mut poll = libc::pollfd {
            fd: probe.as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        // SAFETY: poll points at one initialized descriptor observation.
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 1_000) }, 1);
        assert_ne!(poll.revents & libc::POLLHUP, 0, "received descriptor leaked");
    }
}
