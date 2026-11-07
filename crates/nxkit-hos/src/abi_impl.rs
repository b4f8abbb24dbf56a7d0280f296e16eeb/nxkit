//! The kernel as the services call it: events, sessions, shared memory and handles (`nxkit_abi::kernel::Kernel`).
//!
//! The host registers the kernel once with `nxkit_abi::kernel::set_kernel`. Every method here is one call of the
//! kernel's own API, so a service takes no lock that the game's svcs do not take.

use crate::handle::KObject;
use crate::kernel::Kernel;
use nxkit_abi::kernel::{ResultCode, Session};
use std::sync::Arc;

impl nxkit_abi::kernel::Kernel for Kernel {
    fn create_event(&self) -> Result<(u32, u32), ResultCode> {
        Kernel::create_event(self).map_err(|e| e.raw())
    }

    fn signal_event(&self, handle: u32) -> Result<(), ResultCode> {
        Kernel::signal_event(self, handle).map_err(|e| e.raw())
    }

    fn clear_event(&self, handle: u32) -> Result<(), ResultCode> {
        Kernel::clear_event(self, handle).map_err(|e| e.raw())
    }

    fn add_session(&self, session: Arc<dyn Session>) -> Result<u32, ResultCode> {
        self.handles
            .insert(KObject::Session(session))
            .map_err(|e| e.raw())
    }

    fn create_shared_memory(&self, size: u64) -> Result<(u32, u64), ResultCode> {
        self.make_shared_memory(size).map_err(|e| e.raw())
    }

    fn close_handle(&self, handle: u32) -> Result<(), ResultCode> {
        Kernel::close_handle(self, handle).map_err(|e| e.raw())
    }
}
