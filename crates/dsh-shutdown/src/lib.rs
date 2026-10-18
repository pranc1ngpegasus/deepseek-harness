#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::assert_is_empty,
        clippy::bool_assert_comparison,
        clippy::float_cmp
    )
)]
//! Bounded process shutdown for the long-lived CLI surfaces.
//!
//! One disposal starts at most once, normal completion records the requested
//! exit code, a rejecting disposal forces the exit, and a signal escalates.
//! The machine is pure; the caller owns the clock, the process exit, and the
//! application disposal, so each transition is testable without a process.

/// Maximum grace allowed for the application tree to dispose before process exit.
pub const PROCESS_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;

/// What the caller must do after one transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do.
    None,
    /// Begin application disposal, arming the grace timer.
    StartDispose,
    /// Exit the process immediately with this code.
    ForceExit(i32),
    /// Record this exit code and let the process finish naturally.
    Complete(i32),
}

/// Whether a disposal is running and how the exit will finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No disposal has started.
    Idle,
    /// Disposal is running; a signal-started drain forces the exit after it.
    Draining { force_after_dispose: bool },
    /// Disposal finished, by completion or force.
    Finished,
}

/// The exit the machine has already reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Neither completion nor a forced exit was reported.
    Pending,
    /// A forced exit was reported.
    Forced,
    /// A natural completion was reported.
    Completed,
}

/// The shutdown controller's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownMachine {
    phase: Phase,
    code: i32,
    outcome: Outcome,
}

impl Default for ShutdownMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownMachine {
    /// A machine that has started nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: Phase::Idle,
            code: 0,
            outcome: Outcome::Pending,
        }
    }

    /// Request graceful disposal.
    ///
    /// Repeated calls coalesce onto the first request. After a signal-started
    /// disposal, a normal request still does nothing.
    pub fn shutdown(&mut self, code: i32) -> Action {
        if self.phase != Phase::Idle {
            return Action::None;
        }
        self.phase = Phase::Draining {
            force_after_dispose: false,
        };
        self.code = code;
        Action::StartDispose
    }

    /// Handle a process signal.
    ///
    /// Before disposal starts, this starts it and forces the exit when
    /// disposal resolves. While disposal drains, this forces the exit now.
    pub fn interrupt(&mut self, code: i32) -> Action {
        if self.phase == Phase::Idle {
            self.phase = Phase::Draining {
                force_after_dispose: true,
            };
            self.code = code;
            return Action::StartDispose;
        }
        self.force_exit(code)
    }

    /// Report that disposal resolved.
    pub fn dispose_resolved(&mut self) -> Action {
        let Phase::Draining {
            force_after_dispose,
        } = self.phase
        else {
            return Action::None;
        };
        self.phase = Phase::Finished;
        let code = self.code;
        if force_after_dispose {
            self.force_exit(code)
        } else {
            self.complete(code)
        }
    }

    /// Report that disposal rejected.
    pub fn dispose_rejected(&mut self) -> Action {
        if !matches!(self.phase, Phase::Draining { .. }) {
            return Action::None;
        }
        self.phase = Phase::Finished;
        let code = self.code;
        self.force_exit(code)
    }

    /// Report that the grace period elapsed.
    pub fn timeout(&mut self) -> Action {
        if !matches!(self.phase, Phase::Draining { .. }) {
            return Action::None;
        }
        let code = self.code;
        self.force_exit(code)
    }

    fn force_exit(&mut self, code: i32) -> Action {
        // A completed natural exit still yields to a later signal: the process
        // has not exited yet, and the signal's code is the one to report.
        if self.outcome == Outcome::Forced {
            return Action::None;
        }
        self.outcome = Outcome::Forced;
        self.phase = Phase::Finished;
        Action::ForceExit(code)
    }

    fn complete(&mut self, code: i32) -> Action {
        if self.outcome != Outcome::Pending {
            return Action::None;
        }
        self.outcome = Outcome::Completed;
        self.phase = Phase::Finished;
        Action::Complete(code)
    }

    /// Whether disposal has started.
    #[must_use]
    pub const fn started(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    /// Whether the machine exited by force.
    #[must_use]
    pub const fn forced(&self) -> bool {
        matches!(self.outcome, Outcome::Forced)
    }

    /// Whether the machine recorded a natural completion.
    #[must_use]
    pub const fn completed(&self) -> bool {
        matches!(self.outcome, Outcome::Completed)
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, PROCESS_SHUTDOWN_TIMEOUT_MS, ShutdownMachine};

    #[test]
    fn completes_naturally_after_disposal_resolves() {
        let mut machine = ShutdownMachine::new();
        assert!(!machine.started());
        assert!(!machine.forced());
        assert!(!machine.completed());
        assert_eq!(machine.shutdown(0), Action::StartDispose);
        assert!(machine.started());
        assert_eq!(machine.dispose_resolved(), Action::Complete(0));
        assert!(machine.completed());
        assert!(!machine.forced());
        assert!(machine.started());
    }

    #[test]
    fn forces_exit_when_disposal_rejects() {
        let mut machine = ShutdownMachine::new();
        assert_eq!(machine.shutdown(1), Action::StartDispose);
        assert_eq!(machine.dispose_rejected(), Action::ForceExit(1));
        assert!(machine.forced());
        assert!(!machine.completed());
        assert_eq!(machine.dispose_resolved(), Action::None);
    }

    #[test]
    fn forces_exit_when_the_grace_period_elapses() {
        let mut machine = ShutdownMachine::new();
        assert_eq!(machine.shutdown(0), Action::StartDispose);
        assert_eq!(machine.timeout(), Action::ForceExit(0));
        assert_eq!(machine.dispose_resolved(), Action::None);
        assert_eq!(machine.timeout(), Action::None);
    }

    #[test]
    fn a_signal_forces_a_shutdown_already_stuck_in_disposal() {
        let mut machine = ShutdownMachine::new();
        assert_eq!(machine.shutdown(0), Action::StartDispose);
        assert_eq!(machine.interrupt(130), Action::ForceExit(130));
        assert_eq!(machine.dispose_resolved(), Action::None);
    }

    #[test]
    fn disposal_started_by_a_signal_forces_its_exit_after_resolving() {
        let mut machine = ShutdownMachine::new();
        assert_eq!(machine.interrupt(143), Action::StartDispose);
        assert_eq!(machine.dispose_resolved(), Action::ForceExit(143));
        assert_eq!(machine.shutdown(0), Action::None);
    }

    #[test]
    fn the_first_signal_drains_and_the_second_forces() {
        let mut machine = ShutdownMachine::new();
        assert_eq!(machine.interrupt(143), Action::StartDispose);
        assert_eq!(machine.interrupt(130), Action::ForceExit(130));
        assert_eq!(machine.shutdown(0), Action::None);
    }

    #[test]
    fn coalesces_normal_shutdown_calls_without_escalating() {
        let mut machine = ShutdownMachine::new();
        assert_eq!(machine.shutdown(0), Action::StartDispose);
        assert_eq!(machine.shutdown(1), Action::None);
        assert_eq!(machine.dispose_resolved(), Action::Complete(0));
    }

    #[test]
    fn a_signal_after_natural_completion_still_forces_the_exit() {
        let mut machine = ShutdownMachine::new();
        assert_eq!(machine.shutdown(0), Action::StartDispose);
        assert_eq!(machine.dispose_resolved(), Action::Complete(0));
        assert_eq!(machine.interrupt(130), Action::ForceExit(130));
    }

    #[test]
    fn ignores_disposal_reports_that_never_started() {
        let mut machine = ShutdownMachine::new();
        assert_eq!(machine.dispose_resolved(), Action::None);
        assert_eq!(machine.dispose_rejected(), Action::None);
        assert_eq!(machine.timeout(), Action::None);
        assert!(!machine.started());
    }

    #[test]
    fn the_default_matches_a_fresh_machine() {
        let mut default = ShutdownMachine::default();
        assert_eq!(default.shutdown(7), Action::StartDispose);
        assert_eq!(PROCESS_SHUTDOWN_TIMEOUT_MS, 5_000);
    }

    proptest::proptest! {
        #[test]
        fn force_and_complete_are_each_reported_at_most_once(
            steps in proptest::collection::vec(0u8..6, 0..40),
        ) {
            let mut machine = ShutdownMachine::new();
            let mut forces = 0;
            let mut completes = 0;
            for step in steps {
                let action = match step {
                    0 => machine.shutdown(1),
                    1 => machine.interrupt(130),
                    2 => machine.dispose_resolved(),
                    3 => machine.dispose_rejected(),
                    4 => machine.timeout(),
                    _ => machine.shutdown(0),
                };
                match action {
                    Action::ForceExit(_) => {
                        forces += 1;
                    }
                    Action::Complete(_) => {
                        completes += 1;
                        proptest::prop_assert!(!machine.forced() || machine.completed());
                    }
                    Action::StartDispose | Action::None => {}
                }
            }
            proptest::prop_assert!(forces <= 1);
            proptest::prop_assert!(completes <= 1);
        }

        #[test]
        fn one_shutdown_call_starts_exactly_one_disposal(code in -100i32..200) {
            let mut machine = ShutdownMachine::new();
            proptest::prop_assert_eq!(machine.shutdown(code), Action::StartDispose);
            proptest::prop_assert_eq!(machine.shutdown(code), Action::None);
            proptest::prop_assert_eq!(machine.dispose_resolved(), Action::Complete(code));
            proptest::prop_assert_eq!(machine.shutdown(code), Action::None);
        }
    }
}
