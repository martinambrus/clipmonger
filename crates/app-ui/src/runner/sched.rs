//! Repaint scheduling for the runner. Pure: no window and no egui context.
//!
//! Each window owns one [`Deadline`], the time its next pass is due. The
//! runner arms it from egui repaint requests ([`Deadline::request`]) and from
//! wake and window events ([`Deadline::at`]), runs the passes that are due,
//! clears their deadlines ([`Deadline::clear`]) and then derives the event
//! loop's control flow from all deadlines with [`control_flow`].

use std::time::{Duration, Instant};

use winit::event_loop::ControlFlow;

/// When the next pass of one window is due, if at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Deadline {
    next: Option<Instant>,
}

impl Deadline {
    /// No pass due.
    #[must_use]
    pub const fn new() -> Self {
        Self { next: None }
    }

    /// The earliest time a pass is due; `None` when none is.
    #[must_use]
    pub const fn next(&self) -> Option<Instant> {
        self.next
    }

    /// A pass is due at `when` at the latest. Keeps the sooner of `when` and
    /// the deadline already armed (min-merge).
    pub fn at(&mut self, when: Instant) {
        self.next = Some(match self.next {
            Some(current) => current.min(when),
            None => when,
        });
    }

    /// A pass is due `delay` after `now`, merged like [`Self::at`]. A delay
    /// that does not fit into an `Instant` (egui uses `Duration::MAX` for
    /// "never") arms nothing and must not panic: add with `checked_add`.
    pub fn after(&mut self, now: Instant, delay: Duration) {
        if let Some(when) = now.checked_add(delay) {
            self.at(when);
        }
    }

    /// Handle an egui repaint request. `requested_pass` is the request's
    /// `current_cumulative_pass_nr`; `current_pass` is the context's
    /// `cumulative_pass_nr_for(ROOT)` at the time the request is handled.
    ///
    /// Accepted when `current_pass == requested_pass` or
    /// `current_pass == requested_pass + 1` (no overflow: use `checked_add`);
    /// then the deadline is armed like [`Self::after`] and `true` is returned,
    /// also when `delay` arms nothing. Any other pair (`current_pass` below
    /// `requested_pass`, or two or more above it) is stale: the deadline stays
    /// as it was and `false` is returned.
    pub fn request(
        &mut self,
        now: Instant,
        delay: Duration,
        requested_pass: u64,
        current_pass: u64,
    ) -> bool {
        if current_pass == requested_pass || requested_pass.checked_add(1) == Some(current_pass) {
            self.after(now, delay);
            true
        } else {
            false
        }
    }

    /// A pass is due: a deadline is armed and it is `now` or earlier.
    #[must_use]
    pub fn is_due(&self, now: Instant) -> bool {
        self.next.is_some_and(|t| t <= now)
    }

    /// A pass ran: drop the deadline. A request that arrives later arms it again.
    pub fn clear(&mut self) {
        self.next = None;
    }
}

/// The control flow the event loop is to sleep with: `WaitUntil` the earliest
/// of the armed `deadlines`, or `Wait` when none is armed. Never `Poll`. An
/// earliest deadline in the past gives `WaitUntil` that instant unchanged.
#[must_use]
pub fn control_flow(deadlines: impl IntoIterator<Item = Option<Instant>>) -> ControlFlow {
    deadlines
        .into_iter()
        .flatten()
        .min()
        .map_or(ControlFlow::Wait, ControlFlow::WaitUntil)
}
