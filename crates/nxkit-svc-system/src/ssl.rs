//! The TLS service `ssl`. The host is offline, as nxnative (`dm/host/hle_net.c`, the `nn::ssl` block
//! after the comment "ssl: offline"): a
//! context can be created, but no connection can be created and no certificate can be imported.
//!
//! IDs and layouts are from libnx `ssl.c` and switchbrew `SSL services`.
//!
//! Root: 0 `CreateContext` returns `ISslContext`, 1 `GetContextCount` gives the number of live contexts, 5
//! `SetInterfaceVersion` and 9 `ClearTls12FallbackFlag` are accepted, 101 `SetThreadCoreMask` is stored and 102
//! `GetThreadCoreMask` gives it back.
//!
//! `ISslContext`: 0 `SetOption` is accepted, 3 `GetConnectionCount` gives 0, 2 and 100 `CreateConnection` and 4, 5 and
//! 8 (the certificate import commands) fail with `RES_FAIL`.

use crate::util::{RES_FAIL, fail};
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// `ssl`.
pub(crate) struct Ssl {
    contexts: Arc<AtomicU32>,
    core_mask: AtomicU64,
}

impl Ssl {
    /// A service without contexts.
    pub(crate) fn new() -> Ssl {
        Ssl {
            contexts: Arc::new(AtomicU32::new(0)),
            core_mask: AtomicU64::new(0),
        }
    }
}

impl Service for Ssl {
    fn name(&self) -> &'static str {
        "ssl"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => {
                self.contexts.fetch_add(1, Ordering::SeqCst);
                rsp.objects.push(Arc::new(Context {
                    contexts: self.contexts.clone(),
                }));
            }
            1 => rsp.push_u32(self.contexts.load(Ordering::SeqCst)),
            5 | 9 => {}
            101 => self
                .core_mask
                .store(req.u64_at(0).unwrap_or(0), Ordering::SeqCst),
            102 => rsp.push_u64(self.core_mask.load(Ordering::SeqCst)),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::ssl::sf::ISslContext`.
struct Context {
    contexts: Arc<AtomicU32>,
}

impl Drop for Context {
    fn drop(&mut self) {
        self.contexts.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Service for Context {
    fn name(&self) -> &'static str {
        "ssl::ISslContext"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => {}
            3 => rsp.push_u32(0),
            2 | 4 | 5 | 8 | 100 => return fail(rsp, RES_FAIL),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}
