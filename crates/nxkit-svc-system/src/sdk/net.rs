//! `nn::socket` and `nnsocket*` take-overs that fail offline. Source: nxnative `dm/host/hle_net.c`, the socket section,
//! and the socket overloads of `dm/host/hle_offline.c`.
//!
//! The host creates no socket. Every call that needs a peer returns -1 with the errno `ENETDOWN`, a name lookup gives a
//! null pointer, and `GetLastError` and `GetLastErrno` read the errno of the calling thread. `EventFd`, `Poll`, `Select`
//! and `Close` work and are in `socket.rs`. The plain `pollfd`, `fd_set` and `timeval` overloads of `Poll` and `Select`
//! use the same functions. The plain `Fcntl(int, int, ...)` returns -1 and leaves the errno as it is. The offline module of `nxkit-rt` (`offline/table.rs`) holds the other
//! socket, nifm and ssl names, so this module lists only the names that it lacks. The `Initialize`, `GetAddrInfo` and
//! `FreeAddrInfo` forms and `GetLastErrno` of the offline module answer a fixed `ENETDOWN`, so an event descriptor
//! error (`EAGAIN`, `EBADF`) shows through `GetLastError` and `nnsocketGetLastErrno` only.

use super::neg1;
use super::socket::{self, close, down, errno, event_fd, event_fd_read, event_fd_write, poll, select};

/// A call that fails: -1 and `ENETDOWN`.
extern "C" fn fail_int() -> i32 {
    down()
}

/// A transfer call that fails: -1 and `ENETDOWN`.
extern "C" fn fail_size() -> i64 {
    i64::from(down())
}

/// A name lookup that fails: a null pointer and `ENETDOWN`.
extern "C" fn host_null() -> u64 {
    socket::set_errno(socket::ENETDOWN);
    0
}

extern "C" fn last_error() -> i32 {
    errno()
}

extern "C" fn set_errno(e: u32) {
    socket::set_errno(e as i32);
}

table! {
    "_ZN2nn6socket6SocketENS0_6FamilyENS0_4TypeENS0_8ProtocolE" => fail_int,
    "_ZN2nn6socket4BindEiPKNS0_8SockAddrEj" => fail_int,
    "_ZN2nn6socket7ConnectEiPKNS0_8SockAddrEj" => fail_int,
    "_ZN2nn6socket6ListenEii" => fail_int,
    "_ZN2nn6socket6AcceptEiPNS0_8SockAddrEPj" => fail_int,
    "_ZN2nn6socket5CloseEi" => close,
    "_ZN2nn6socket11GetPeerNameEiPNS0_8SockAddrEPj" => fail_int,
    "_ZN2nn6socket11GetPeerNameEiP8sockaddrPj" => fail_int,
    "_ZN2nn6socket11GetSockNameEiPNS0_8SockAddrEPj" => fail_int,
    "_ZN2nn6socket10GetSockOptEiNS0_5LevelENS0_6OptionEPvPj" => fail_int,
    "_ZN2nn6socket10SetSockOptEiNS0_5LevelENS0_6OptionEPKvj" => fail_int,
    "_ZN2nn6socket5FcntlEiNS0_12FcntlCommandEz" => fail_int,
    // offline (cut): the variadic Fcntl of SDK 11 returns -1 and leaves the errno as it is.
    "_ZN2nn6socket5FcntlEiiz" => neg1,
    "_ZN2nn6socket8ShutdownEiNS0_14ShutdownMethodE" => fail_int,
    "_ZN2nn6socket4PollEPNS0_6PollFdEmi" => poll,
    // offline: waits out the timeout when there is nothing to wait on, else fails (the plain `pollfd` form)
    "_ZN2nn6socket4PollEP6pollfdmi" => poll,
    "poll" => poll,
    "_ZN2nn6socket6SelectEiPNS0_5FdSetES2_S2_PNS0_7TimeValE" => select,
    // offline: as above, with the plain `fd_set` and `timeval` of SDK 4 (Dark Souls Remastered)
    "_ZN2nn6socket6SelectEiP6fd_setS2_S2_P7timeval" => select,
    "_ZN2nn6socket7EventFdEmNS0_12EventFdFlagsE" => event_fd,
    "_ZN2nn6socket11EventFdReadEiPm" => event_fd_read,
    "_ZN2nn6socket12EventFdWriteEim" => event_fd_write,
    "_ZN2nn6socket17ResolverGetOptionEPNS0_14ResolverOptionENS0_17ResolverOptionKeyE" => fail_int,
    "_ZN2nn6socket17ResolverSetOptionERKNS0_14ResolverOptionE" => fail_int,
    "_ZN2nn6socket4ReadEiPvm" => fail_size,
    "_ZN2nn6socket5WriteEiPKvm" => fail_size,
    "_ZN2nn6socket4RecvEiPvmNS0_7MsgFlagE" => fail_size,
    "_ZN2nn6socket8RecvFromEiPvmNS0_7MsgFlagEPNS0_8SockAddrEPj" => fail_size,
    "_ZN2nn6socket4SendEiPKvmNS0_7MsgFlagE" => fail_size,
    "_ZN2nn6socket16GetHostEntByNameEPKc" => host_null,
    "_ZN2nn6socket16GetHostEntByNameEPKcPKNS0_14ResolverOptionEm" => host_null,
    "_ZN2nn6socket12GetLastErrorEv" => last_error,
    "nnsocketSocket" => fail_int,
    "nnsocketBind" => fail_int,
    "nnsocketConnect" => fail_int,
    "nnsocketClose" => close,
    "nnsocketGetPeerName" => fail_int,
    "nnsocketGetSockName" => fail_int,
    "nnsocketGetSockOpt" => fail_int,
    "nnsocketSetSockOpt" => fail_int,
    "nnsocketFcntl" => fail_int,
    "nnsocketPoll" => poll,
    "nnsocketResolverGetOption" => fail_int,
    "nnsocketResolverSetOption" => fail_int,
    "nnsocketRecv" => fail_size,
    "nnsocketSend" => fail_size,
    "nnsocketGetHostByNameWithOptions" => host_null,
    "nnsocketGetLastErrno" => last_error,
    "nnsocketSetLastErrno" => set_errno,
}
