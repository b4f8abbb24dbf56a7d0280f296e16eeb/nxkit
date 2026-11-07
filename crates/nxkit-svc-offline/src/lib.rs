//! The `offline` service of nxkit: what the game gets for networking and peripherals it cannot use here.
//!
//! The service has SDK-level functions only (R14) and no IPC interface, so [`services`] is empty. The functions are in
//! the module `sys`, the one module of the crate that may use `unsafe`, because they write through pointers that the
//! game passes.

use nxkit_abi::export::Export;
use nxkit_abi::ipc::Named;

mod sys;

/// The SDK-level functions of the service, by Switch mangled name (R14).
pub fn exports() -> Vec<Export> {
    sys::entries()
}

/// The IPC services of the service. There are none.
pub fn services() -> Vec<Named> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_has_exports_and_no_ipc() {
        assert!(!exports().is_empty());
        assert!(services().is_empty());
    }
}
