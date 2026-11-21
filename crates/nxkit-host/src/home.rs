//! The Home button of a controller: a short press opens the Home menu (Resume, Back to the launcher, Quit), holding it
//! for [`HOLD_S`] opens the quit prompt. The logic of the dm front end's `home_quit.h` (NX2Apple), with the focus of
//! NX2Apple's console mode: the prompt's focus starts on the safe answer, left and right move it, A takes the focused
//! answer and B or Home closes. Pure, so it is tested without a window or a controller.

/// How long Home is held before the prompt opens, in seconds.
pub const HOLD_S: f64 = 2.0;
/// The hold indicator shows only after this, so a short press shows nothing.
pub const RING_S: f64 = 0.3;
/// Home let go within this is a short press: it opens the menu.
pub const TAP_S: f64 = 0.5;

/// Where the Home button is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Nothing shown, the game has its input.
    Idle,
    /// Home is held: the indicator fills up.
    Holding,
    /// The quit prompt is open: the game gets no input.
    Prompt,
    /// A screen closed: the game gets input again once Home, A and B are all up.
    Settle,
    /// Quit was taken: the host ends the game (final).
    Quit,
    /// The Home menu is open: the game gets no input.
    Menu,
    /// Back to the launcher was taken: the game is paused in the background until [`HomeMenu::resumed`].
    Paused,
}

/// A row of the Home menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Close the menu.
    Resume,
    /// Pause the game in the background and show the launcher.
    Launcher,
    /// Open the quit prompt.
    Quit,
}

/// The buttons of every controller together, as they are now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons {
    /// Home (or `-` and `+` together).
    pub home: bool,
    /// The button labelled A.
    pub a: bool,
    /// The button labelled B.
    pub b: bool,
    /// Up on the d-pad or the left stick.
    pub up: bool,
    /// Down.
    pub down: bool,
    /// Left.
    pub left: bool,
    /// Right.
    pub right: bool,
}

/// What the host turned on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// A short press opens the menu (`NXKIT_HOME_MENU`, on unless `0`).
    pub menu: bool,
    /// Holding Home opens the quit prompt (`NXKIT_HOME_QUIT`, on unless `0`).
    pub hold: bool,
    /// The menu has Back to the launcher (`NXKIT_LAUNCHER_BUNDLE_ID` is set).
    pub launcher: bool,
}

/// The focus the game is told while a screen of the Home button is up, as the console's HOME menu tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameFocus {
    /// The game is in front again.
    In,
    /// A screen covers the game (the menu or the prompt).
    Out,
    /// The game is paused in the background (Back to the launcher).
    Background,
}

/// What the window needs for the Home button: the options, the texts and the launcher to show on Back to the
/// launcher (its bundle identifier).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    /// What is on.
    pub options: Options,
    /// The texts in the player's language.
    pub texts: Texts,
    /// The bundle identifier of the launcher (`NXKIT_LAUNCHER_BUNDLE_ID`).
    pub launcher: Option<String>,
}

impl Setup {
    /// The setup of the run: the menu and the hold as the settings say, Back to the launcher when a launcher is
    /// named, the texts from `NXKIT_HOME_TEXT` or the English ones for `game`. `None` when both are off.
    pub fn new(
        menu: bool,
        hold: bool,
        launcher: Option<String>,
        text: Option<&str>,
        game: &str,
    ) -> Option<Setup> {
        let options = Options {
            menu,
            hold,
            launcher: launcher.is_some(),
        };
        (menu || hold).then(|| Setup {
            options,
            texts: Texts::from_lines(text, game),
            launcher,
        })
    }
}

/// The quit prompt's answers in the order shown: Cancel, then Quit.
pub const ANSWER_CANCEL: usize = 0;
/// The quit prompt's Quit.
pub const ANSWER_QUIT: usize = 1;

/// The state of the Home button.
#[derive(Debug, Clone)]
pub struct HomeMenu {
    phase: Phase,
    since: f64,
    last: Buttons,
    a_up: bool,
    b_up: bool,
    sel: usize,
    answer: usize,
    opts: Options,
}

impl HomeMenu {
    /// Nothing shown.
    pub fn new(opts: Options) -> Self {
        HomeMenu {
            phase: Phase::Idle,
            since: 0.0,
            last: Buttons::default(),
            a_up: false,
            b_up: false,
            sel: 0,
            answer: ANSWER_CANCEL,
            opts,
        }
    }

    /// The Home button does anything at all.
    pub fn enabled(&self) -> bool {
        self.opts.menu || self.opts.hold
    }

    /// Where it is.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The rows of the menu, in the order shown.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Resume];
        if self.opts.launcher {
            rows.push(Row::Launcher);
        }
        rows.push(Row::Quit);
        rows
    }

    /// The index of the focused row.
    pub fn selected(&self) -> usize {
        self.sel
    }

    /// The focused answer of the prompt ([`ANSWER_CANCEL`] or [`ANSWER_QUIT`]).
    pub fn answer(&self) -> usize {
        self.answer
    }

    fn open(&mut self, phase: Phase, b: Buttons) {
        self.phase = phase;
        self.a_up = !b.a;
        self.b_up = !b.b;
        self.sel = 0;
        self.answer = ANSWER_CANCEL;
    }

    /// One step with the buttons as they are at `now` (seconds of a clock that only goes forward).
    pub fn step(&mut self, now: f64, b: Buttons) -> Phase {
        let last = std::mem::replace(&mut self.last, b);
        let pressed = b.home && !last.home;
        let (up, down) = (b.up && !last.up, b.down && !last.down);
        let (left, right) = (b.left && !last.left, b.right && !last.right);
        match self.phase {
            Phase::Idle if pressed && self.enabled() => {
                self.phase = Phase::Holding;
                self.since = now;
            }
            Phase::Holding if !b.home => {
                if self.opts.menu && now - self.since < TAP_S {
                    self.open(Phase::Menu, b);
                } else {
                    self.phase = Phase::Idle;
                }
            }
            Phase::Holding if self.opts.hold && now - self.since >= HOLD_S => {
                self.open(Phase::Prompt, b);
            }
            Phase::Menu => {
                self.a_up |= !b.a;
                self.b_up |= !b.b;
                let rows = self.rows();
                if (b.b && self.b_up) || pressed {
                    self.phase = Phase::Settle;
                } else if b.a && self.a_up {
                    match rows[self.sel.min(rows.len() - 1)] {
                        Row::Resume => self.phase = Phase::Settle,
                        Row::Launcher => self.phase = Phase::Paused,
                        Row::Quit => {
                            // The A that chose Quit does not answer the prompt.
                            self.open(Phase::Prompt, b);
                            self.a_up = false;
                        }
                    }
                } else if up && !down && self.sel > 0 {
                    self.sel -= 1;
                } else if down && !up && self.sel + 1 < rows.len() {
                    self.sel += 1;
                }
            }
            Phase::Prompt => {
                self.a_up |= !b.a;
                self.b_up |= !b.b;
                if (b.b && self.b_up) || pressed {
                    self.phase = Phase::Settle;
                } else if b.a && self.a_up {
                    self.phase = if self.answer == ANSWER_QUIT {
                        Phase::Quit
                    } else {
                        Phase::Settle
                    };
                } else if left && !right {
                    self.answer = ANSWER_CANCEL;
                } else if right && !left {
                    self.answer = ANSWER_QUIT;
                }
            }
            Phase::Settle if !b.home && !b.a && !b.b => self.phase = Phase::Idle,
            _ => {}
        }
        self.phase
    }

    /// The window lost the focus: whatever was shown closes, and a Home still held needs a new press. A game paused
    /// in the background stays paused.
    pub fn cancel(&mut self) {
        if !matches!(self.phase, Phase::Quit | Phase::Paused) {
            self.phase = Phase::Idle;
            self.last.home = true;
        }
    }

    /// The paused game is shown again: it gets input once Home, A and B are up.
    pub fn resumed(&mut self) {
        if self.phase == Phase::Paused {
            self.phase = Phase::Settle;
            self.last.home = true;
        }
    }

    /// The game gets no controller, keyboard or mouse input now.
    pub fn blocks_input(&self) -> bool {
        matches!(
            self.phase,
            Phase::Prompt | Phase::Settle | Phase::Quit | Phase::Menu | Phase::Paused
        )
    }

    /// How full the hold indicator is (0 to 1), `None` while it is not shown.
    pub fn ring(&self, now: f64) -> Option<f64> {
        if self.phase != Phase::Holding || !self.opts.hold {
            return None;
        }
        let t = (now - self.since - RING_S) / (HOLD_S - RING_S);
        (t >= 0.0).then(|| t.min(1.0))
    }
}

/// The texts of the screens, in the order of the lines of `NXKIT_HOME_TEXT` (the order of the dm front end's
/// `DM_HOME_QUIT_TEXT`, so a launcher passes the same lines): hold, title, note, quit, cancel, resume, launcher, menu
/// title. An empty or missing line takes the English default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texts {
    /// The hold indicator.
    pub hold: String,
    /// The prompt's title.
    pub title: String,
    /// The prompt's note.
    pub note: String,
    /// Quit (the prompt's answer and the menu's row).
    pub quit: String,
    /// Cancel.
    pub cancel: String,
    /// Resume.
    pub resume: String,
    /// Back to the launcher.
    pub launcher: String,
    /// The menu's title.
    pub menu_title: String,
}

impl Texts {
    /// The texts of `lines` (one per line), the defaults naming `game`.
    pub fn from_lines(lines: Option<&str>, game: &str) -> Texts {
        let given: Vec<&str> = lines.map(|l| l.split('\n').collect()).unwrap_or_default();
        let pick = |i: usize, def: String| {
            given
                .get(i)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map_or(def, str::to_owned)
        };
        Texts {
            hold: pick(0, "Keep holding to quit".into()),
            title: pick(1, format!("Quit {game}?")),
            note: pick(2, "Unsaved progress will be lost.".into()),
            quit: pick(3, "Quit".into()),
            cancel: pick(4, "Cancel".into()),
            resume: pick(5, "Resume".into()),
            launcher: pick(6, "Back to the launcher".into()),
            menu_title: pick(7, game.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Options = Options {
        menu: true,
        hold: true,
        launcher: true,
    };
    const HOME: Buttons = Buttons {
        home: true,
        a: false,
        b: false,
        up: false,
        down: false,
        left: false,
        right: false,
    };
    const NONE: Buttons = Buttons {
        home: false,
        a: false,
        b: false,
        up: false,
        down: false,
        left: false,
        right: false,
    };

    fn with(f: impl FnOnce(&mut Buttons)) -> Buttons {
        let mut b = NONE;
        f(&mut b);
        b
    }

    #[test]
    fn a_short_press_opens_the_menu() {
        let mut m = HomeMenu::new(ALL);
        assert_eq!(m.step(0.0, HOME), Phase::Holding);
        assert_eq!(m.ring(0.1), None, "nothing shows for a short press");
        assert_eq!(m.step(0.2, NONE), Phase::Menu);
        assert!(m.blocks_input());
        assert_eq!(m.rows(), vec![Row::Resume, Row::Launcher, Row::Quit]);
    }

    #[test]
    fn holding_opens_the_prompt_on_cancel() {
        let mut m = HomeMenu::new(ALL);
        m.step(0.0, HOME);
        assert!(m.ring(1.0).is_some_and(|r| r > 0.0 && r < 1.0));
        assert_eq!(m.step(2.1, HOME), Phase::Prompt);
        assert_eq!(m.answer(), ANSWER_CANCEL);
        assert_eq!(
            m.step(2.2, with(|b| b.a = true)),
            Phase::Settle,
            "A on Cancel closes"
        );
        assert_eq!(m.step(2.3, NONE), Phase::Idle);
    }

    #[test]
    fn quit_needs_right_then_a() {
        let mut m = HomeMenu::new(ALL);
        m.step(0.0, HOME);
        m.step(2.1, HOME);
        m.step(2.2, NONE);
        m.step(2.3, with(|b| b.right = true));
        assert_eq!(m.answer(), ANSWER_QUIT);
        assert_eq!(m.step(2.4, with(|b| b.a = true)), Phase::Quit);
    }

    #[test]
    fn quit_in_the_menu_asks_first() {
        let mut m = HomeMenu::new(ALL);
        m.step(0.0, HOME);
        m.step(0.1, NONE);
        m.step(0.2, with(|b| b.down = true));
        m.step(0.3, NONE);
        m.step(0.4, with(|b| b.down = true));
        assert_eq!(m.selected(), 2);
        assert_eq!(m.step(0.5, with(|b| b.a = true)), Phase::Prompt);
        assert_eq!(
            m.step(0.6, with(|b| b.a = true)),
            Phase::Prompt,
            "the A that chose Quit is not an answer"
        );
    }

    #[test]
    fn back_to_the_launcher_pauses_until_resumed() {
        let mut m = HomeMenu::new(ALL);
        m.step(0.0, HOME);
        m.step(0.1, NONE);
        m.step(0.2, with(|b| b.down = true));
        assert_eq!(m.step(0.3, with(|b| b.a = true)), Phase::Paused);
        m.cancel();
        assert_eq!(m.phase(), Phase::Paused, "hiding the game takes its focus");
        m.resumed();
        assert_eq!(m.phase(), Phase::Settle);
        assert_eq!(m.step(1.0, NONE), Phase::Idle);
    }

    #[test]
    fn b_or_home_closes_and_no_launcher_row_without_one() {
        let mut m = HomeMenu::new(Options {
            launcher: false,
            ..ALL
        });
        assert_eq!(m.rows(), vec![Row::Resume, Row::Quit]);
        m.step(0.0, HOME);
        m.step(0.1, NONE);
        assert_eq!(m.step(0.2, with(|b| b.b = true)), Phase::Settle);
        let mut m = HomeMenu::new(ALL);
        m.step(0.0, HOME);
        m.step(0.1, NONE);
        assert_eq!(m.step(0.2, HOME), Phase::Settle);
    }

    #[test]
    fn off_does_nothing() {
        let mut m = HomeMenu::new(Options {
            menu: false,
            hold: false,
            launcher: false,
        });
        assert!(!m.enabled());
        assert_eq!(m.step(0.0, HOME), Phase::Idle);
    }

    #[test]
    fn the_setup_names_the_launcher_row() {
        let s = Setup::new(true, true, Some("org.example".into()), None, "Zelda");
        assert!(s.is_some_and(|s| s.options.launcher && s.texts.menu_title == "Zelda"));
        assert!(Setup::new(true, false, None, None, "Zelda").is_some_and(|s| !s.options.launcher));
        assert_eq!(Setup::new(false, false, None, None, "Zelda"), None);
    }

    #[test]
    fn texts_take_the_lines_and_fill_the_rest() {
        let t = Texts::from_lines(Some("Halten\n\n\nBeenden"), "Zelda");
        assert_eq!(t.hold, "Halten");
        assert_eq!(t.title, "Quit Zelda?");
        assert_eq!(t.quit, "Beenden");
        assert_eq!(t.menu_title, "Zelda");
    }
}
