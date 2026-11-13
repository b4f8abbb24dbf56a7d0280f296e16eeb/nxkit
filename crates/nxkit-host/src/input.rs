//! Platform-independent mouse event vocabulary for host input seams.

/// What the held mouse did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEvent {
    /// It moved by `dx`, `dy` points, y down.
    Moved {
        /// To the right.
        dx: f64,
        /// Down.
        dy: f64,
    },
    /// A button went down or up. 0 is left, 1 right, 2 middle.
    Button {
        /// The AppKit button number.
        number: u8,
        /// Down or up.
        down: bool,
    },
    /// The wheel or the trackpad scrolled by `dy`, positive away from the user.
    Wheel {
        /// The scroll distance.
        dy: f64,
    },
    /// The window let go of the mouse. Every held button counts as up.
    Released,
}

/// When the window holds the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    /// Never. The game gets no mouse.
    Off,
    /// After a click into the window, for example while a controller is connected.
    Click,
    /// Whenever the window is the key window, and after a click.
    Auto,
}
