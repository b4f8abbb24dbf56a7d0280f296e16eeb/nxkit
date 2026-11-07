//! The main thread's settings from a game's `main.npdm`.
//!
//! META header offsets (Switchbrew, NPDM page, written from memory and not yet checked against a real file):
//! magic `META` at 0, main thread priority (u8) at 0xE, default core (u8) at 0xF, main thread stack size (u32) at 0x1C.

/// What the NPDM says about the main thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MainThread {
    /// The priority (0 to 63, lower runs first).
    pub priority: u8,
    /// The core the thread starts on.
    pub core: u8,
    /// The stack size in bytes.
    pub stack_size: u32,
}

/// Reads the main thread's settings from the raw NPDM.
pub fn main_thread(npdm: &[u8]) -> Result<MainThread, String> {
    if npdm.len() < 0x20 || &npdm[..4] != b"META" {
        return Err("main.npdm: no META header".into());
    }
    let stack = [npdm[0x1c], npdm[0x1d], npdm[0x1e], npdm[0x1f]];
    let t = MainThread {
        priority: npdm[0xe],
        core: npdm[0xf],
        stack_size: u32::from_le_bytes(stack),
    };
    if t.priority > 63 || t.core > 3 || t.stack_size == 0 || t.stack_size % 0x1000 != 0 {
        return Err(format!(
            "main.npdm: the main thread settings are not plausible (priority {}, core {}, stack {:#x}): the header offsets need a check",
            t.priority, t.core, t.stack_size
        ));
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(priority: u8, core: u8, stack: u32) -> Vec<u8> {
        let mut h = vec![0u8; 0x80];
        h[..4].copy_from_slice(b"META");
        h[0xe] = priority;
        h[0xf] = core;
        h[0x1c..0x20].copy_from_slice(&stack.to_le_bytes());
        h
    }

    #[test]
    fn reads_the_thread_settings() {
        let t = main_thread(&header(44, 0, 0x100000)).unwrap();
        assert_eq!(
            t,
            MainThread {
                priority: 44,
                core: 0,
                stack_size: 0x100000
            }
        );
    }

    #[test]
    fn refuses_a_file_without_the_header_or_with_odd_values() {
        assert!(main_thread(&[0; 8]).is_err());
        assert!(main_thread(&header(70, 0, 0x100000)).is_err());
        assert!(main_thread(&header(44, 0, 0)).is_err());
    }
}
