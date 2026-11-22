//! The input verbs of the control socket (W36): `press`, `hold`, `release`, `stick`, `input`, `record` and `replay`.
//!
//! They add input for the game, record what player 1 gives it, and play a recording. The service does the work
//! (`nxkit_svc_input::live`). This part only names the verbs. The commands are in `docs/diagnostics.md`.

use crate::control::Control;

/// The verbs, their usage and what they do.
const VERBS: [(&str, &str, &str); 7] = [
    (
        "press BUTTONS [MS] [player N]",
        "press",
        "presses buttons for MS ms (100), then lets them go",
    ),
    (
        "hold BUTTONS [player N]",
        "hold",
        "holds buttons until release",
    ),
    (
        "release [BUTTONS] [player N]",
        "release",
        "lets the buttons go, or everything",
    ),
    (
        "stick l|r X Y [MS] [player N]",
        "stick",
        "sets a stick, -1 to 1, Y up. Without MS it stays",
    ),
    (
        "input COMMAND",
        "input",
        "one line of the input script (press, hold, release, stick)",
    ),
    (
        "record start FILE [frames|ms] | stop",
        "record",
        "writes what player 1 gives the game as an input script",
    ),
    (
        "replay FILE | stop",
        "replay",
        "plays an input script now, over the controllers",
    ),
];

/// Adds the input verbs to the control.
pub(crate) fn add(control: &mut Control) {
    for (usage, verb, help) in VERBS {
        control.verb(
            usage,
            help,
            Box::new(move |rest| nxkit_svc_input::live::command(verb, rest)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_abi::live::Board;

    #[test]
    fn the_verbs_are_known_to_the_control_and_fail_with_a_reason() {
        let mut c = Control::new(Board::new(Default::default(), Vec::new()), Vec::new());
        add(&mut c);
        let help = c.answer("help").unwrap_or_default().join("\n");
        for (_, verb, _) in VERBS {
            assert!(help.contains(verb), "{verb}");
        }
        assert!(c.answer("press").is_err());
        assert!(c.answer("input wait 5").is_err());
        assert!(c.answer("record stop").is_err());
    }
}
