//! The boot of the SDK on the main guest thread (R57).
//!
//! On the console `rtld` and `nn::init::Start` start a game. The host has neither, so it runs their steps itself, in the
//! order of `dm/host/main.c` of the engine: `_init_libc0`, `nnosInitialize`, `_init_libc1`, `nninitInitializeSdkModule`,
//! `SetHostArgc` and `SetHostArgv`, `nninitStartup`, `_init_libc2`, the `DT_INIT` function of each module, then `nnMain`.
//! The `entry` of `game.json` is not used. The offset 0 of a module is its header and holds no code.
//!
//! Each step is a call of one guest function with [`gate::enter`] on the main stack. The call returns when the
//! function returns. A symbol that the exports lack stops the host before the window opens, with its name (R15).

use std::ffi::{CString, c_char};

use crate::error::{HostError, Result};
use crate::loader::ModuleInit;
use crate::{gate, guestsym};

const INIT_LIBC0: &str = "_init_libc0";
const OS_INITIALIZE: &str = "nnosInitialize";
const INIT_LIBC1: &str = "_init_libc1";
const INIT_SDK_MODULE: &str = "nninitInitializeSdkModule";
const SET_HOST_ARGC: &str = "_ZN2nn2os11SetHostArgcEi";
const SET_HOST_ARGV: &str = "_ZN2nn2os11SetHostArgvEPPc";
const INIT_STARTUP: &str = "nninitStartup";
const INIT_LIBC2: &str = "_init_libc2";
const NN_MAIN: &str = "nnMain";

/// The one argument of the game, as the engine passes it.
const ARGV0: &str = "game";

/// A guest function that the boot calls, with its address.
#[derive(Debug, Clone, Copy)]
struct Func {
    name: &'static str,
    addr: u64,
}

/// The guest functions and the argument vector of the boot.
#[derive(Debug)]
pub struct Boot {
    libc0: Func,
    os_initialize: Func,
    libc1: Func,
    sdk_module: Func,
    set_argc: Func,
    set_argv: Func,
    startup: Func,
    libc2: Func,
    nn_main: Func,
    inits: Vec<ModuleInit>,
    /// The address of the `char *argv[2]` array. The memory is never freed, because the SDK keeps the pointers for the
    /// whole run.
    argv: u64,
}

fn symbol(name: &'static str) -> Result<Func> {
    match guestsym::find(name) {
        Some(addr) => Ok(Func { name, addr }),
        None => Err(HostError::Load(format!(
            "the boot of the SDK needs the symbol {name}, and no module exports it (R15)"
        ))),
    }
}

/// Makes `argv = { "game", NULL }` and leaks it. It returns the address of the array.
fn leak_argv() -> Result<u64> {
    let arg = CString::new(ARGV0).map_err(|e| HostError::Load(format!("argv[0]: {e}")))?;
    let argv: &'static mut [*mut c_char; 2] =
        Box::leak(Box::new([arg.into_raw(), std::ptr::null_mut()]));
    Ok(argv.as_mut_ptr() as usize as u64)
}

impl Boot {
    /// Looks up every guest function of the boot in the symbols of the modules. [`guestsym::set`] must have run.
    pub fn resolve(inits: Vec<ModuleInit>) -> Result<Boot> {
        Ok(Boot {
            libc0: symbol(INIT_LIBC0)?,
            os_initialize: symbol(OS_INITIALIZE)?,
            libc1: symbol(INIT_LIBC1)?,
            sdk_module: symbol(INIT_SDK_MODULE)?,
            set_argc: symbol(SET_HOST_ARGC)?,
            set_argv: symbol(SET_HOST_ARGV)?,
            startup: symbol(INIT_STARTUP)?,
            libc2: symbol(INIT_LIBC2)?,
            nn_main: symbol(NN_MAIN)?,
            inits,
            argv: leak_argv()?,
        })
    }

    fn call(f: Func, x0: u64, x1: u64, sp: u64) {
        eprintln!(">>> {}", f.name);
        gate::enter(f.addr, x0, x1, sp);
    }

    /// Runs the boot and `nnMain` on the calling host thread, which must be the attached main guest thread with its
    /// thread slots set up. `main_handle` is the kernel handle of the thread. `sp` is the top of the main stack, and
    /// every call starts there, because the call before it has returned.
    pub fn run(&self, main_handle: u32, sp: u64) {
        Self::call(self.libc0, 0, 0, sp);
        Self::call(self.os_initialize, u64::from(main_handle), 0, sp);
        Self::call(self.libc1, 0, 0, sp);
        Self::call(self.sdk_module, 0, 0, sp);
        Self::call(self.set_argc, 1, 0, sp);
        Self::call(self.set_argv, self.argv, 0, sp);
        Self::call(self.startup, 0, 0, sp);
        Self::call(self.libc2, 0, 0, sp);
        for i in &self.inits {
            eprintln!(">>> DT_INIT {} +{:#x}", i.module, i.offset);
            gate::enter(i.addr, 0, 0, sp);
        }
        Self::call(self.nn_main, 0, 0, sp);
        eprintln!("nnMain returned");
    }
}
