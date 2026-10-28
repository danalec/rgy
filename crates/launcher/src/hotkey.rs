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
//!
//! A window holds one gesture. Four presses in a row are one gesture and
//! not two: the pair that completes it fires, and the presses that follow
//! inside the same window belong to the gesture that fired.

/// The window between the two presses, in milliseconds.
///
/// Four hundred milliseconds: a deliberate double-tap is well inside it,
/// and two Control presses an inch apart on the keyboard are not.
pub const DEFAULT_WINDOW_MS: u64 = 400;

/// Where the machine is in the gesture.
///
/// A window holds one gesture: the pair that completes it fires, and the
/// presses that follow inside the same window belong to the gesture that
/// fired and do not fire another one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    /// No press waiting: the next press starts a pair.
    #[default]
    Idle,
    /// One press waiting: the next press inside the window completes the
    /// pair.
    Armed,
    /// The gesture fired: the window is spent.
    Fired,
}

/// The state machine behind the double-Control hotkey.
#[derive(Debug, Clone, Default)]
pub struct DoubleCtrl {
    /// The window between the two presses.
    window_ms: u64,
    /// When the last press was, if there was one.
    last: Option<u64>,
    /// Where the machine is in the gesture.
    state: State,
}

impl DoubleCtrl {
    /// A machine with the default window.
    pub fn new() -> DoubleCtrl {
        DoubleCtrl {
            window_ms: DEFAULT_WINDOW_MS,
            last: None,
            state: State::Idle,
        }
    }

    /// A machine with a window of the caller's choosing.
    pub fn with_window(window_ms: u64) -> DoubleCtrl {
        DoubleCtrl {
            window_ms,
            last: None,
            state: State::Idle,
        }
    }

    /// The window between the two presses, in milliseconds.
    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }

    /// Feed a Control press at `now_ms`.
    ///
    /// True when this press completes the pair. The gesture fires once per
    /// window: the presses that follow it inside the window are part of
    /// the gesture that fired and do not fire another one.
    pub fn press(&mut self, now_ms: u64) -> bool {
        // The clock has to go forward for a pair to be a pair: a clock that
        // jumped back cannot be trusted, and the safe answer to that is "no
        // gesture".
        let within = match self.last {
            Some(last) => now_ms >= last && now_ms - last <= self.window_ms,
            None => false,
        };
        if !within {
            // The window is closed: the press is the first of a new pair,
            // and whatever came before it is over.
            self.last = Some(now_ms);
            self.state = State::Armed;
            return false;
        }
        match self.state {
            // The gesture fired and the window is still open: this press
            // belongs to the gesture that fired, and fires nothing —
            // otherwise four quick presses open and close the window twice.
            State::Fired => false,
            // The pair is complete: the gesture fires, and the rest of the
            // window is spent.
            State::Armed => {
                self.last = Some(now_ms);
                self.state = State::Fired;
                true
            }
            // A window is open only because a press opened it, so there is
            // no pair to complete: the safe answer is "no gesture".
            State::Idle => false,
        }
    }

    /// Forget the pending press.
    ///
    /// The hook calls this for every key that is not Control: a Control
    /// pressed, then a letter, then a Control, is not a double Control.
    pub fn interrupt(&mut self) {
        self.last = None;
        self.state = State::Idle;
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
    fn four_presses_inside_the_window_fire_once() {
        let mut d = DoubleCtrl::new();
        assert!(!d.press(1000), "the first press is half a gesture");
        assert!(d.press(1100), "the second press completes it");
        assert!(!d.press(1200), "the third press is part of the gesture");
        assert!(
            !d.press(1300),
            "and so is the fourth: one gesture, one fire"
        );
    }

    #[test]
    fn a_pair_after_the_window_fires_again() {
        let mut d = DoubleCtrl::new();
        d.press(1000);
        assert!(d.press(1100));
        // The window is spent, and a pair after it is a new gesture.
        assert!(!d.press(1100 + DEFAULT_WINDOW_MS + 1));
        assert!(d.press(1100 + DEFAULT_WINDOW_MS + 100));
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
        assert!(
            !d.press(1100),
            "a letter in between is not a double Control"
        );
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
