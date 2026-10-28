// SPDX-License-Identifier: Apache-2.0

//! The global hotkey: Control pressed twice.
//!
//! `RegisterHotKey` cannot do this: a hotkey needs a modifier, and the
//! only modifier the user wants here is Control on its own. So the hotkey
//! is a **low-level keyboard hook**, which sees the key before the window
//! does, and the state machine that decides whether two presses in a row
//! were the gesture or two people mashing Control.
//!
//! The window between the two presses is the whole decision. Too long and
//! it fires when the user means something else; too short and a slightly
//! clumsy double-tap misses. The value here is in the middle of what a
//! person does on purpose, and it is measured in the clock the caller
//! provides — the model does not read a clock, because a model that reads
//! one cannot be tested.

/// The window between the two presses, in milliseconds.
///
/// Four hundred milliseconds: a deliberate double-tap is well inside it,
/// and two Control presses an inch apart on the keyboard are not.
pub const DEFAULT_WINDOW_MS: u64 = 400;

/// The state machine behind the double-Control hotkey.
#[derive(Debug, Clone, Default)]
pub struct DoubleCtrl {
    /// The window between the two presses.
    window_ms: u64,
    /// When the last press was, if there was one.
    last: Option<u64>,
    /// Whether the last press was the first of a pair.
    armed: bool,
}

impl DoubleCtrl {
    /// A machine with the default window.
    pub fn new() -> DoubleCtrl {
        DoubleCtrl {
            window_ms: DEFAULT_WINDOW_MS,
            last: None,
            armed: false,
        }
    }

    /// A machine with a window of the caller's choosing.
    pub fn with_window(window_ms: u64) -> DoubleCtrl {
        DoubleCtrl {
            window_ms,
            last: None,
            armed: false,
        }
    }

    /// The window between the two presses, in milliseconds.
    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }

    /// Feed a Control press at `now_ms`.
    ///
    /// True when this press completes the pair. The machine is then disarmed
    /// until the next press, so one gesture fires once and not twice.
    pub fn press(&mut self, now_ms: u64) -> bool {
        // The clock has to go forward for a pair to be a pair: a clock that
        // jumped back cannot be trusted, and the safe answer to that is "no
        // gesture".
        let within = match self.last {
            Some(last) => now_ms >= last && now_ms - last <= self.window_ms,
            None => false,
        };
        let fired = within && self.armed;
        self.last = Some(now_ms);
        // A gesture that fired is over: the next press starts a new one, so
        // one double-Control fires once and not twice.
        self.armed = !fired;
        fired
    }

    /// Forget the pending press.
    ///
    /// The hook calls this for every key that is not Control: a Control
    /// pressed, then a letter, then a Control, is not a double Control.
    pub fn interrupt(&mut self) {
        self.last = None;
        self.armed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_presses_inside_the_window_fire_once() {
        let mut d = DoubleCtrl::new();
        assert!(!d.press(1000), "the first press is half a gesture");
        assert!(d.press(1200), "the second press completes it");
    }

    #[test]
    fn a_third_press_does_not_fire_again() {
        let mut d = DoubleCtrl::new();
        d.press(1000);
        assert!(d.press(1100));
        assert!(!d.press(1200), "one gesture fires once");
    }

    #[test]
    fn two_presses_outside_the_window_do_not_fire() {
        let mut d = DoubleCtrl::new();
        assert!(!d.press(1000));
        assert!(!d.press(1000 + DEFAULT_WINDOW_MS + 1));
    }

    #[test]
    fn the_window_is_inclusive_at_its_edge() {
        let mut d = DoubleCtrl::new();
        d.press(1000);
        assert!(
            d.press(1000 + DEFAULT_WINDOW_MS),
            "the window is closed, so its edge still counts"
        );
    }

    #[test]
    fn another_key_breaks_the_pair() {
        let mut d = DoubleCtrl::new();
        d.press(1000);
        d.interrupt();
        assert!(!d.press(1100), "a letter in between is not a double Control");
    }

    #[test]
    fn the_clock_going_backwards_does_not_fire() {
        // A clock that jumps back is a clock that cannot be trusted, and the
        // safe answer to that is "no gesture".
        let mut d = DoubleCtrl::new();
        d.press(1000);
        assert!(!d.press(500));
    }
}
