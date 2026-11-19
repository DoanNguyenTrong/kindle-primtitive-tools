//! Auto-rotation: turns raw orientation readings into a stable screen rotation. A new orientation is
//! accepted only after it has been read continuously for `hold_ms`. Any reading that is not a clear
//! orientation (flat, unreadable) resets the timer, so a Kindle being tilted back and forth does not flip
//! the screen.
//!
//! The readings come from the accelerometer (`bma2x2`): its sysfs `value` file gives x, y and z, about 1024 per
//! g, and answers with the stock UI stopped (S4, device-notes.md). Its input events stay off without the stock
//! UI, so the launcher polls `value`.

use crate::settings::Rotation;

/// Below this (about 0.6 g) on the y axis the Kindle is too flat or too sideways to tell.
const Y_MIN: i32 = 600;

/// The turn one accelerometer reading shows, or `None` when it is not clear: flat on a table, sideways, or
/// between positions. Measured on the Oasis 3: upright reads y from +900 to +990, upside down about −1000, flat
/// z about +1030, sideways x about ±1000.
pub fn turn_from_accel(x: i32, y: i32, _z: i32) -> Option<Rotation> {
    // y must be clear of 0.6 g and at least 1.5 times x, so a Kindle half turned sideways gives no reading.
    if y.abs() < Y_MIN || 2 * y.abs() < 3 * x.abs() {
        return None;
    }
    Some(if y > 0 { Rotation::Upright } else { Rotation::UpsideDown })
}

/// The three numbers of the accelerometer's `value` file ("5 905 458").
pub fn parse_accel(text: &str) -> Option<(i32, i32, i32)> {
    let mut n = text.split_whitespace().map(|v| v.parse::<i32>().ok());
    Some((n.next()??, n.next()??, n.next()??))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Debouncer {
    current: Rotation,
    candidate: Option<(Rotation, u64)>,
    hold_ms: u64,
}

impl Debouncer {
    pub fn new(current: Rotation, hold_ms: u64) -> Debouncer {
        Debouncer { current, candidate: None, hold_ms }
    }

    pub fn current(&self) -> Rotation {
        self.current
    }

    /// Feed one reading at `now_ms` (any monotonic clock, in milliseconds). Returns the new rotation when it
    /// changes.
    pub fn feed(&mut self, reading: Option<Rotation>, now_ms: u64) -> Option<Rotation> {
        let Some(seen) = reading else {
            self.candidate = None;
            return None;
        };
        if seen == self.current {
            self.candidate = None;
            return None;
        }
        match self.candidate {
            Some((wanted, since)) if wanted == seen => {
                if now_ms.saturating_sub(since) >= self.hold_ms {
                    self.current = seen;
                    self.candidate = None;
                    Some(seen)
                } else {
                    None
                }
            }
            _ => {
                self.candidate = Some((seen, now_ms));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Rotation::{UpsideDown, Upright};

    #[test]
    fn readings_measured_on_the_kindle_give_the_right_turn() {
        // Held upright, leaning back a little; upside down; upright again (S4, 2026-11-18).
        assert_eq!(turn_from_accel(5, 900, 459), Some(Upright));
        assert_eq!(turn_from_accel(194, -997, 287), Some(UpsideDown));
        assert_eq!(turn_from_accel(103, 987, 250), Some(Upright));
        // Flat on the table, sideways both ways, and halfway through a turn: no reading.
        assert_eq!(turn_from_accel(71, -4, 1032), None);
        assert_eq!(turn_from_accel(1085, -56, 9), None);
        assert_eq!(turn_from_accel(-690, -575, 448), None);
        assert_eq!(turn_from_accel(-768, -446, 485), None);
    }

    #[test]
    fn the_value_file_is_three_numbers() {
        assert_eq!(parse_accel("5 905 458\n"), Some((5, 905, 458)));
        assert_eq!(parse_accel("-690 -575 448"), Some((-690, -575, 448)));
        assert_eq!(parse_accel("1 2"), None);
        assert_eq!(parse_accel("a b c"), None);
    }

    #[test]
    fn a_steady_turn_is_accepted_after_the_hold_time() {
        let mut d = Debouncer::new(Upright, 800);
        assert_eq!(d.feed(Some(UpsideDown), 0), None);
        assert_eq!(d.feed(Some(UpsideDown), 500), None);
        assert_eq!(d.feed(Some(UpsideDown), 800), Some(UpsideDown));
        assert_eq!(d.current(), UpsideDown);
    }

    #[test]
    fn a_flapping_reading_never_flips_the_screen() {
        let mut d = Debouncer::new(Upright, 800);
        for t in (0..5000).step_by(100) {
            let r = if (t / 100) % 2 == 0 { UpsideDown } else { Upright };
            assert_eq!(d.feed(Some(r), t), None, "flipped at {t} ms");
        }
        assert_eq!(d.current(), Upright);
    }

    #[test]
    fn an_unclear_reading_restarts_the_hold() {
        let mut d = Debouncer::new(Upright, 800);
        d.feed(Some(UpsideDown), 0);
        d.feed(Some(UpsideDown), 600);
        assert_eq!(d.feed(None, 700), None); // flat: the candidate is dropped
        assert_eq!(d.feed(Some(UpsideDown), 900), None); // a new hold starts here
        assert_eq!(d.feed(Some(UpsideDown), 1600), None);
        assert_eq!(d.feed(Some(UpsideDown), 1700), Some(UpsideDown));
    }

    #[test]
    fn the_current_orientation_cancels_a_pending_change() {
        let mut d = Debouncer::new(Upright, 800);
        d.feed(Some(UpsideDown), 0);
        d.feed(Some(Upright), 300);
        assert_eq!(d.feed(Some(UpsideDown), 400), None);
        assert_eq!(d.feed(Some(UpsideDown), 1100), None); // only 700 ms since the restart
        assert_eq!(d.feed(Some(UpsideDown), 1200), Some(UpsideDown));
    }

    #[test]
    fn a_clock_going_backwards_does_not_commit() {
        let mut d = Debouncer::new(Upright, 800);
        d.feed(Some(UpsideDown), 5000);
        assert_eq!(d.feed(Some(UpsideDown), 4000), None);
    }
}
