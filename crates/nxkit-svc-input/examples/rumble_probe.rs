//! Plays a short rumble pattern on the first connected controller and prints its name and how it rumbles.
//! `cargo run -p nxkit-svc-input --example rumble_probe`, or with `-- --no-hd` to keep a Switch Pro Controller on
//! Core Haptics. It runs on the main thread, whose run loop GameController needs to report the controllers.

fn main() {
    let hd = !std::env::args().any(|a| a == "--no-hd");
    if !nxkit_svc_input::probe::rumble_probe(hd, &mut |line| println!("{line}")) {
        std::process::exit(1);
    }
}
