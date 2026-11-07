//! The Horizon system services that a game opens at startup: settings, performance, time, account, applet, parental
//! controls, network, friends, telemetry, logging, battery, TLS, BCAT and add-on content.
//!
//! The services are `nxkit_abi::ipc::Service` objects. The IPC server (`nxkit-ipc`) owns the sessions and the
//! dispatch, and this crate answers the commands (R37). The command IDs and layouts are from libnx and switchbrew. The
//! values follow nxnative, which answers the same questions at the SDK level (`dm/host/`). Every module names its
//! sources. A command that no module lists is [`Handled::Unknown`](nxkit_abi::ipc::Handled::Unknown), and the server stops
//! the game (R15).
//!
//! The host drives the applet state with [`set_focus`], [`set_operation_mode`] and [`push_message`].

mod acc;
mod apm;
mod applet;
mod applet_ifaces;
mod bcat;
mod friend;
mod lm;
mod nifm;
mod pctl;
mod set;
mod ssl;
mod state;
mod sys;
mod time;
mod tz;
mod util;

pub use state::{Focus, push_message, set_focus, set_operation_mode};

use nxkit_abi::export::Export;
use nxkit_abi::ipc::Named;
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
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Sets the configuration. The first call wins. A service that runs before `init` uses the defaults.
pub fn init(config: &Config) {
    let _ = CONFIG.set(config.clone());
}

/// The configuration that `init` set, or the defaults.
pub(crate) fn config() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

/// The functions that this crate gives the game in place of SDK functions.
///
/// There are none. nxnative takes over the `nn::` functions of the SDK, but an nxkit game keeps its SDK module and
/// reaches the services through IPC (R14, R37).
pub fn exports() -> Vec<Export> {
    Vec::new()
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
    fn there_are_no_sdk_exports() {
        assert!(exports().is_empty());
    }
}
