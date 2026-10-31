//! Repaint scheduling of the runner: arming, the pass-number accept rule,
//! overflow safety and the control flow derived from all deadlines.
//!
//! Run: `cargo test -p clipmonger-app-ui --test runner_sched`

use std::time::{Duration, Instant};

use clipmonger_app_ui::runner::sched::{control_flow, Deadline};
use winit::event_loop::ControlFlow;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// An instant that is safely in the past when the caller uses it.
fn past_instant() -> Instant {
    let past = Instant::now();
    std::thread::sleep(ms(5));
    past
}

/// Accept rule table for one requested pass number: (current pass, accepted).
fn pass_cases(nr: u64) -> Vec<(u64, bool)> {
    let mut cases = vec![
        (nr, true),
        (nr + 1, true),
        (nr + 2, false),
        (nr + 3, false),
        (nr + 1000, false),
    ];
    if nr > 0 {
        cases.push((nr - 1, false));
        cases.push((0, false));
    }
    if nr > 1 {
        cases.push((nr - 2, false));
    }
    cases
}

#[test]
fn new_deadline_is_unarmed() {
    let now = Instant::now();
    let d = Deadline::new();
    assert_eq!(d.next(), None);
    assert!(!d.is_due(now));
    assert!(!d.is_due(now + ms(60_000)));
    assert_eq!(d, Deadline::default());
}

#[test]
fn at_arms_the_deadline() {
    let when = Instant::now() + ms(40);
    let mut d = Deadline::new();
    d.at(when);
    assert_eq!(d.next(), Some(when));
}

#[test]
fn at_keeps_the_sooner_deadline_in_both_orders() {
    let now = Instant::now();
    let (early, late) = (now + ms(10), now + ms(50));
    let mut late_first = Deadline::new();
    late_first.at(late);
    late_first.at(early);
    assert_eq!(late_first.next(), Some(early), "sooner pulls in");
    let mut early_first = Deadline::new();
    early_first.at(early);
    early_first.at(late);
    assert_eq!(early_first.next(), Some(early), "later must not push out");
    let mut same = Deadline::new();
    same.at(early);
    same.at(early);
    assert_eq!(same.next(), Some(early));
}

#[test]
fn after_adds_the_delay_to_now_and_min_merges() {
    let now = Instant::now();
    let mut d = Deadline::new();
    d.after(now, ms(30));
    assert_eq!(d.next(), Some(now + ms(30)));
    d.after(now, ms(80));
    assert_eq!(
        d.next(),
        Some(now + ms(30)),
        "longer delay must not push out"
    );
    d.after(now, ms(5));
    assert_eq!(d.next(), Some(now + ms(5)), "shorter delay pulls in");
    d.at(now + ms(2));
    assert_eq!(d.next(), Some(now + ms(2)), "at merges with after");
    d.after(now, ms(9));
    assert_eq!(d.next(), Some(now + ms(2)), "after merges with at");
}

#[test]
fn after_with_unrepresentable_delay_arms_nothing() {
    let now = Instant::now();
    let mut d = Deadline::new();
    d.after(now, Duration::MAX);
    assert_eq!(d.next(), None);
    assert!(!d.is_due(now + ms(60_000)));
    d.at(now + ms(20));
    d.after(now, Duration::MAX);
    assert_eq!(
        d.next(),
        Some(now + ms(20)),
        "armed deadline stays as it was"
    );
}

#[test]
fn zero_delay_is_due_at_once() {
    let now = Instant::now();
    let mut after = Deadline::new();
    after.after(now, Duration::ZERO);
    assert_eq!(after.next(), Some(now));
    assert!(after.is_due(now));
    let mut request = Deadline::new();
    assert!(request.request(now, Duration::ZERO, 4, 4));
    assert_eq!(request.next(), Some(now));
    assert!(request.is_due(now));
}

#[test]
fn is_due_boundary_is_inclusive() {
    let when = Instant::now() + ms(100);
    let mut d = Deadline::new();
    d.at(when);
    let early = when
        .checked_sub(Duration::from_nanos(1))
        .expect("instant in range");
    assert!(!d.is_due(early), "1 ns early");
    assert!(d.is_due(when), "exactly at the deadline");
    assert!(d.is_due(when + Duration::from_nanos(1)));
    assert!(d.is_due(when + ms(10_000)));
}

#[test]
fn clear_drops_the_deadline_and_a_later_request_arms_it_again() {
    let now = Instant::now();
    let mut d = Deadline::new();
    d.at(now);
    assert!(d.is_due(now));
    d.clear();
    assert_eq!(d.next(), None);
    assert!(!d.is_due(now + ms(60_000)));
    assert!(d.request(now, ms(25), 9, 9));
    assert_eq!(d.next(), Some(now + ms(25)));
    // No memory of the cleared deadline: a later one arms as given.
    let mut e = Deadline::new();
    e.at(now + ms(5));
    e.clear();
    e.at(now + ms(500));
    assert_eq!(e.next(), Some(now + ms(500)));
}

#[test]
fn request_accept_rule_and_return_values() {
    let now = Instant::now();
    for nr in [0_u64, 1, 2, 7, 1_000_000, 1 << 40] {
        for (current, accepted) in pass_cases(nr) {
            let mut d = Deadline::new();
            assert_eq!(
                d.request(now, ms(30), nr, current),
                accepted,
                "requested {nr}, current {current}"
            );
            let expected = accepted.then(|| now + ms(30));
            assert_eq!(d.next(), expected, "requested {nr}, current {current}");
        }
    }
}

#[test]
fn stale_request_leaves_an_armed_deadline_untouched() {
    let now = Instant::now();
    let armed = now + ms(100);
    for nr in [3_u64, 50] {
        for (current, accepted) in pass_cases(nr) {
            for delay in [Duration::ZERO, ms(10), Duration::MAX] {
                let mut d = Deadline::new();
                d.at(armed);
                assert_eq!(d.request(now, delay, nr, current), accepted);
                let expected = match (accepted, delay) {
                    (true, delay) if delay != Duration::MAX => now + delay,
                    _ => armed,
                };
                assert_eq!(
                    d.next(),
                    Some(expected),
                    "requested {nr}, current {current}, delay {delay:?}"
                );
            }
        }
    }
}

#[test]
fn request_pass_numbers_do_not_overflow() {
    let now = Instant::now();
    let mut d = Deadline::new();
    assert!(d.request(now, ms(10), u64::MAX, u64::MAX));
    assert_eq!(d.next(), Some(now + ms(10)));
    let mut d = Deadline::new();
    assert!(!d.request(now, ms(10), u64::MAX, 0), "no wrap around to 0");
    assert!(!d.request(now, ms(10), u64::MAX, u64::MAX - 1));
    assert!(!d.request(now, ms(10), u64::MAX - 1, 0));
    assert_eq!(d.next(), None);
    assert!(d.request(now, ms(10), u64::MAX - 1, u64::MAX));
    assert_eq!(d.next(), Some(now + ms(10)));
}

#[test]
fn request_with_unrepresentable_delay_is_accepted_and_arms_nothing() {
    let now = Instant::now();
    let mut d = Deadline::new();
    assert!(
        d.request(now, Duration::MAX, 5, 5),
        "accepted, nothing armed"
    );
    assert_eq!(d.next(), None);
    assert!(d.request(now, Duration::MAX, 5, 6));
    assert_eq!(d.next(), None);
    assert!(!d.request(now, Duration::MAX, 5, 9));
    assert_eq!(d.next(), None);
    d.at(now + ms(20));
    assert!(d.request(now, Duration::MAX, 5, 5));
    assert_eq!(d.next(), Some(now + ms(20)));
}

#[test]
fn request_merges_with_the_armed_deadline() {
    let now = Instant::now();
    let mut d = Deadline::new();
    assert!(d.request(now, ms(50), 1, 1));
    assert!(d.request(now, ms(20), 2, 2));
    assert_eq!(d.next(), Some(now + ms(20)));
    assert!(d.request(now, ms(80), 3, 3));
    assert_eq!(d.next(), Some(now + ms(20)));
    d.at(now + ms(5));
    assert!(d.request(now, ms(15), 4, 5));
    assert_eq!(d.next(), Some(now + ms(5)));
}

#[test]
fn control_flow_waits_when_nothing_is_armed() {
    assert_eq!(
        control_flow(Vec::<Option<Instant>>::new()),
        ControlFlow::Wait
    );
    assert_eq!(control_flow(std::iter::empty()), ControlFlow::Wait);
    assert_eq!(control_flow([None, None]), ControlFlow::Wait);
    let unarmed = [Deadline::new(), Deadline::new()];
    assert_eq!(
        control_flow(unarmed.iter().map(Deadline::next)),
        ControlFlow::Wait
    );
}

#[test]
fn control_flow_waits_until_the_earliest_deadline() {
    let now = Instant::now();
    let (a, b, c) = (now + ms(10), now + ms(20), now + ms(30));
    for order in [
        [a, b, c],
        [a, c, b],
        [b, a, c],
        [b, c, a],
        [c, a, b],
        [c, b, a],
    ] {
        assert_eq!(
            control_flow(order.map(Some)),
            ControlFlow::WaitUntil(a),
            "{order:?}"
        );
    }
    assert_eq!(control_flow([Some(c)]), ControlFlow::WaitUntil(c));
    assert_eq!(control_flow([Some(a), Some(a)]), ControlFlow::WaitUntil(a));
    assert_eq!(
        control_flow([None, Some(c), None, Some(b)]),
        ControlFlow::WaitUntil(b)
    );
}

#[test]
fn control_flow_reads_the_deadlines_of_several_windows() {
    let now = Instant::now();
    let mut popup = Deadline::new();
    let mut settings = Deadline::new();
    popup.at(now + ms(40));
    settings.at(now + ms(15));
    let windows = [popup, settings];
    assert_eq!(
        control_flow(windows.iter().map(Deadline::next)),
        ControlFlow::WaitUntil(now + ms(15))
    );
}

#[test]
fn control_flow_keeps_a_past_deadline_unchanged() {
    let past = past_instant();
    let future = Instant::now() + ms(500);
    assert!(past < Instant::now());
    assert_eq!(control_flow([Some(past)]), ControlFlow::WaitUntil(past));
    assert_eq!(
        control_flow([Some(future), Some(past)]),
        ControlFlow::WaitUntil(past)
    );
    assert_eq!(
        control_flow([None, Some(past), Some(future)]),
        ControlFlow::WaitUntil(past)
    );
}

#[test]
fn control_flow_is_never_poll() {
    let past = past_instant();
    let now = Instant::now();
    let future = now + ms(500);
    let inputs: Vec<Vec<Option<Instant>>> = vec![
        vec![],
        vec![None],
        vec![None, None],
        vec![Some(past)],
        vec![Some(now)],
        vec![Some(future)],
        vec![Some(past), Some(future)],
        vec![None, None, Some(future)],
    ];
    for input in inputs {
        let flow = control_flow(input.clone());
        assert_ne!(flow, ControlFlow::Poll, "{input:?}");
        let expected = input
            .iter()
            .flatten()
            .min()
            .map_or(ControlFlow::Wait, |&t| ControlFlow::WaitUntil(t));
        assert_eq!(flow, expected, "{input:?}");
    }
}

/// Two windows, a virtual clock and the loop's tick: run what is due, clear
/// it, derive the control flow from what is left.
struct Loop {
    base: Instant,
    now: Instant,
    windows: [Deadline; 2],
    passes: [u64; 2],
}

impl Loop {
    fn new() -> Self {
        let base = Instant::now();
        Self {
            base,
            now: base,
            windows: [Deadline::new(), Deadline::new()],
            passes: [0, 0],
        }
    }

    fn at(&self, offset_ms: u64) -> Instant {
        self.base + ms(offset_ms)
    }

    fn advance_to(&mut self, offset_ms: u64) {
        self.now = self.at(offset_ms);
    }

    /// Hands a repaint request to the window's deadline against its own
    /// completed-pass counter, as the loop does.
    fn request(&mut self, window: usize, delay_ms: u64, requested_pass: u64) -> bool {
        let current = self.passes[window];
        self.windows[window].request(self.now, ms(delay_ms), requested_pass, current)
    }

    /// Runs the due passes, clears them and returns the flow the loop would
    /// sleep with; it must never be a stale `WaitUntil` or `Poll`.
    fn tick(&mut self) -> ControlFlow {
        for (deadline, passes) in self.windows.iter_mut().zip(&mut self.passes) {
            if deadline.is_due(self.now) {
                *passes += 1;
                deadline.clear();
            }
        }
        let flow = control_flow(self.windows.iter().map(Deadline::next));
        match flow {
            ControlFlow::Poll => panic!("Poll after a tick"),
            ControlFlow::WaitUntil(when) => {
                assert!(when > self.now, "stale WaitUntil({when:?}) after a tick");
            }
            ControlFlow::Wait => {}
        }
        flow
    }
}

#[test]
fn tick_runs_a_due_window_once_and_goes_back_to_waiting() {
    let mut l = Loop::new();
    assert!(l.request(0, 50, 0));
    assert_eq!(l.tick(), ControlFlow::WaitUntil(l.at(50)));
    l.advance_to(49);
    assert_eq!(l.tick(), ControlFlow::WaitUntil(l.at(50)));
    assert_eq!(l.passes, [0, 0]);
    l.advance_to(50);
    assert_eq!(l.tick(), ControlFlow::Wait);
    assert_eq!(l.passes, [1, 0]);
    l.advance_to(900);
    assert_eq!(l.tick(), ControlFlow::Wait);
    assert_eq!(l.passes, [1, 0], "a cleared deadline does not run again");
}

#[test]
fn tick_with_two_windows_sleeps_until_the_next_one() {
    let mut l = Loop::new();
    assert!(l.request(0, 10, 0));
    assert!(l.request(1, 30, 0));
    assert_eq!(l.tick(), ControlFlow::WaitUntil(l.at(10)));
    l.advance_to(10);
    assert_eq!(l.tick(), ControlFlow::WaitUntil(l.at(30)));
    assert_eq!(l.passes, [1, 0]);
    l.advance_to(31);
    assert_eq!(l.tick(), ControlFlow::Wait);
    assert_eq!(l.passes, [1, 1], "a late wake runs the overdue window");
}

#[test]
fn tick_runs_every_overdue_window_in_one_go() {
    let mut l = Loop::new();
    l.windows[0].at(l.at(5));
    l.windows[1].at(l.at(8));
    l.advance_to(20);
    assert_eq!(l.tick(), ControlFlow::Wait);
    assert_eq!(l.passes, [1, 1]);
}

#[test]
fn tick_animation_rearms_after_each_pass() {
    let mut l = Loop::new();
    assert!(l.request(0, 0, 0));
    for frame in 1..=5_u64 {
        assert_eq!(l.tick(), ControlFlow::Wait, "pass {frame} ran and cleared");
        assert_eq!(l.passes[0], frame);
        // Asked for during that pass, handled after it completed.
        assert!(l.request(0, 16, frame - 1));
        assert_eq!(l.tick(), ControlFlow::WaitUntil(l.now + ms(16)));
        l.advance_to(frame * 16);
    }
    assert_eq!(l.passes[1], 0);
}

#[test]
fn tick_ignores_a_request_a_pass_has_served() {
    let mut l = Loop::new();
    assert!(l.request(0, 5, 0));
    l.advance_to(5);
    assert_eq!(l.tick(), ControlFlow::Wait);
    assert_eq!(l.passes[0], 1);
    // Two more passes happen, then requests from before them show up.
    for _ in 0..2 {
        l.windows[0].at(l.now);
        assert_eq!(l.tick(), ControlFlow::Wait);
    }
    assert_eq!(l.passes[0], 3);
    assert!(!l.request(0, 0, 0), "served long ago");
    assert!(!l.request(0, 0, 1));
    assert_eq!(l.tick(), ControlFlow::Wait, "no spurious wake-up");
    assert_eq!(l.passes[0], 3);
    // The pass just before the current one and the current one still count.
    assert!(l.request(0, 7, 3));
    assert!(l.request(0, 9, 2));
    assert_eq!(l.tick(), ControlFlow::WaitUntil(l.now + ms(7)));
}

/// Small deterministic generator for the differential test.
struct Rng(u64);

impl Rng {
    fn bits(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.bits() % n
    }
}

fn merge(plain: &mut Option<Instant>, when: Instant) {
    *plain = Some(plain.map_or(when, |m| m.min(when)));
}

#[test]
fn deadline_matches_plain_bookkeeping_over_random_operations() {
    let base = Instant::now();
    let probes = [base, base + ms(40), base + ms(120), base + ms(400)];
    let passes = [0, 1, 5, u64::MAX - 1, u64::MAX];
    for seed in 1..=20_u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut d = Deadline::new();
        let mut plain: Option<Instant> = None;
        for step in 0..400 {
            let what = match rng.below(5) {
                0 => {
                    let when = base + ms(rng.below(300));
                    d.at(when);
                    merge(&mut plain, when);
                    "at"
                }
                1 => {
                    let delay = ms(rng.below(300));
                    d.after(base, delay);
                    merge(&mut plain, base + delay);
                    "after"
                }
                2 => {
                    d.after(base, Duration::MAX);
                    "after MAX"
                }
                3 => {
                    let nr = passes[usize::try_from(rng.below(5)).expect("index")];
                    let current = match rng.below(6) {
                        0 => nr,
                        1 => nr.wrapping_add(1),
                        2 => nr.wrapping_add(2),
                        3 => nr.wrapping_sub(1),
                        4 => 0,
                        _ => u64::MAX,
                    };
                    let delay = if rng.below(4) == 0 {
                        Duration::MAX
                    } else {
                        ms(rng.below(300))
                    };
                    let accepted = current == nr || nr.checked_add(1) == Some(current);
                    assert_eq!(
                        d.request(base, delay, nr, current),
                        accepted,
                        "seed {seed} step {step}: request nr {nr} current {current}"
                    );
                    if let Some(when) = base.checked_add(delay).filter(|_| accepted) {
                        merge(&mut plain, when);
                    }
                    "request"
                }
                _ => {
                    d.clear();
                    plain = None;
                    "clear"
                }
            };
            assert_eq!(d.next(), plain, "seed {seed} step {step} after {what}");
            for probe in probes {
                assert_eq!(
                    d.is_due(probe),
                    plain.is_some_and(|m| m <= probe),
                    "seed {seed} step {step} after {what}, probe {probe:?}"
                );
            }
        }
    }
}
