//! The Horizon system services that a game opens at startup: settings, performance, time, account, applet, parental
//! controls, network, friends, telemetry, logging, battery, TLS, BCAT and add-on content. It also takes over the
//! `nn::*` functions of the game SDK that reach those services and a few more (Mii, software keyboard, web, shared
//! font, Opus, NRO loader, GameChat).
//!
//! The services are `nxkit_abi::ipc::Service` objects. The IPC server (`nxkit-ipc`) owns the sessions and the
//! dispatch, and this crate answers the commands (R37). The command IDs and layouts are from libnx and switchbrew. The
//! values follow the engine, which answers the same questions at the SDK level (`dm/host/`). Every module names its
//! sources. A command that no module lists is [`Handled::Unknown`](nxkit_abi::ipc::Handled::Unknown), and the server stops
//! the game (R15).
//!
//! [`exports()`] lists the SDK functions that the host runs in place of the game's own (R38a). An export wins over the
//! SDK function, so the SDK code never opens the IPC session. The IPC services answer the SDK code that no export
//! replaces. [`big_stack_exports()`] lists the exports that the engine runs with `HLE_FORCE_BIG`, for the host to wrap.
//! The exports need a few functions of the host, which the host passes in [`set_seams`].
//!
//! The host drives the applet state with [`set_focus`], [`set_operation_mode`] and [`push_message`].

mod acc;
mod apm;
mod applet;
mod applet_ifaces;
mod bcat;
mod event;
mod friend;
mod lm;
mod nifm;
mod pctl;
mod sdk;
mod seams;
mod set;
mod ssl;
mod state;
mod sys;
mod time;
mod tz;
mod util;

pub use sdk::quit;
pub use seams::{Seams, TextRequest, set_seams};
pub use set::console_language;
pub use state::{Focus, push_message, set_focus, set_operation_mode};

use nxkit_abi::export::Export;
use nxkit_abi::ipc::Named;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// What the host tells the services about the player and the console.
#[derive(Clone, Debug)]
pub struct Config {
    /// The language tag of the console, for example `en-US`. It sets the language list, the region and the desired
    /// language of the game.
    pub language: String,
    /// The nickname of the one user.
    pub nickname: String,
    /// Whether the console is in handheld mode. A docked console is the default.
    pub handheld: bool,
    /// Whether the performance mode is the boost mode. The default follows the operation mode.
    pub boost: bool,
    /// A fixed POSIX time in seconds for the user clock. `None` follows the clock of the host.
    pub fixed_time: Option<i64>,
    /// The launch parameter of the game, returned by `PopLaunchParameter`. `None` means the game has none.
    pub launch_parameter: Option<Vec<u8>>,
    /// The display version of the game, returned by `GetDisplayVersion`.
    pub display_version: String,
    /// The index of the program that ran before this one (`nn::oe::GetPreviousProgramIndex`). `None` gives -1.
    pub previous_program: Option<i32>,
    /// The directory of the add-on content. Each numeric sub directory is the RomFS of one DLC. `None` owns none.
    pub aoc_dir: Option<PathBuf>,
    /// The directory with the shared font files (`standard.ttf`, `nintendo_ext.ttf`). `None` uses the paths below.
    pub font_dir: Option<PathBuf>,
    /// The file of the standard shared font. It wins over `font_dir`.
    pub shared_font: Option<PathBuf>,
    /// The file of the extended shared font. It wins over `font_dir`.
    pub shared_font_ext: Option<PathBuf>,
    /// Whether the game decodes the shared font with `ScalableFontEngineHelper::Decode`. The font address is then the
    /// BFTTF header, else the TTF behind it. A title module says so (`RuntimeHooks::shared_font_decode`).
    pub shared_font_decode: bool,
    /// The names of the services that the game opens through `nn::sf::hipc` and that the host answers with zeroed
    /// output. A title module lists them (`RuntimeHooks::sf_services`).
    pub sf_services: Vec<String>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            language: "en-US".to_owned(),
            nickname: "Player".to_owned(),
            handheld: false,
            boost: false,
            fixed_time: None,
            launch_parameter: None,
            display_version: "1.0.0".to_owned(),
            previous_program: None,
            aoc_dir: None,
            font_dir: None,
            shared_font: None,
            shared_font_ext: None,
            shared_font_decode: false,
            sf_services: Vec::new(),
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Sets the configuration and notes the start of the program. The first call wins. A service that runs before `init`
/// uses the defaults.
pub fn init(config: &Config) {
    let _ = CONFIG.set(config.clone());
    sdk::mark_start();
}

/// The configuration that `init` set, or the defaults.
pub(crate) fn config() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

/// The functions that this crate runs in place of SDK functions of the game (R38a).
///
/// Every export is a forced take-over: it applies even when the module of the game defines the symbol. The
/// exports that the engine runs with `HLE_FORCE_BIG` are in [`big_stack_exports()`] and not here.
pub fn exports() -> Vec<Export> {
    sdk::all()
}

/// The exports that need a bigger guest stack than the game thread has. The host wraps them (the engine
/// `HLE_FORCE_BIG`). They are not in [`exports()`].
pub fn big_stack_exports() -> Vec<Export> {
    sdk::big_stack()
}

/// Tells the crate that the game closed a kernel handle. An event of the SDK layer that was bound to it can be bound
/// again.
pub fn on_handle_closed(handle: u32) {
    event::unbind_handle(handle);
}

/// The services of this crate under their Horizon names.
pub fn services() -> Vec<Named> {
    fn named(name: &'static str, root: Arc<dyn nxkit_abi::ipc::Service>) -> Named {
        Named { name, root }
    }

    vec![
        named("set", Arc::new(set::Settings { system: false })),
        named("set:sys", Arc::new(set::Settings { system: true })),
        named("apm", Arc::new(apm::Apm { name: "apm" })),
        named("apm:p", Arc::new(apm::Apm { name: "apm:p" })),
        named("time:u", Arc::new(time::Time { name: "time:u" })),
        named("time:a", Arc::new(time::Time { name: "time:a" })),
        named("time:s", Arc::new(time::Time { name: "time:s" })),
        named("acc:u0", Arc::new(acc::Account { name: "acc:u0" })),
        named("acc:u1", Arc::new(acc::Account { name: "acc:u1" })),
        named("appletOE", Arc::new(applet::Applet { name: "appletOE" })),
        named("appletAE", Arc::new(applet::Applet { name: "appletAE" })),
        named("pctl", Arc::new(pctl::Pctl { name: "pctl" })),
        named("pctl:a", Arc::new(pctl::Pctl { name: "pctl:a" })),
        named("pctl:s", Arc::new(pctl::Pctl { name: "pctl:s" })),
        named("pctl:r", Arc::new(pctl::Pctl { name: "pctl:r" })),
        named("nifm:u", Arc::new(nifm::Nifm)),
        named("friend:u", Arc::new(friend::Friend)),
        named("prepo:u", Arc::new(lm::Prepo)),
        named("lm", Arc::new(lm::Lm)),
        named("psm", Arc::new(apm::Psm)),
        named("ssl", Arc::new(ssl::Ssl::new())),
        named("bcat:u", Arc::new(bcat::Bcat)),
        named("aoc:u", Arc::new(bcat::Aoc::new())),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_fit_the_service_name_limit_and_are_unique() {
        let services = services();
        let mut names: Vec<&str> = services.iter().map(|s| s.name).collect();
        assert!(names.iter().all(|n| n.len() <= 8));
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), services.len());
    }

    #[test]
    fn the_root_names_match_the_registered_names() {
        for s in services() {
            assert_eq!(s.root.name(), s.name);
        }
    }

    #[test]
    fn the_exports_are_not_empty_and_the_big_ones_are_apart() {
        let all = exports();
        assert!(!all.is_empty());
        let big = big_stack_exports();
        assert!(big.iter().all(|b| all.iter().all(|e| e.name != b.name)));
    }
}
