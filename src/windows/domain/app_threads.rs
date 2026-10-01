//! Which running applications have their Accessibility thread, and when one without gets it.
//!
//! Every sighting of an application asks the same question: its launch, each property observer that
//! reports, and every time it becomes frontmost. The rules are in `specs/windows.md`, "Taking on an
//! application".

use std::collections::HashSet;

use rini_core::ids::pid_t;

/// What macOS reports about an application at the moment rini looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub regular: bool,
    pub finished_launching: bool,
    /// Taken on whatever its activation policy.
    pub login_window: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    Spawn,
    AwaitActivationPolicy,
    AwaitFinishedLaunching,
    AlreadyRunning,
}

#[derive(Debug, Default)]
pub struct AppThreads {
    running: HashSet<pid_t>,
    discovered: bool,
}

impl AppThreads {
    /// What to do about an application rini has just seen. `Spawn` records the thread as running.
    pub fn next(&mut self, pid: pid_t, readiness: Readiness) -> Next {
        if self.running.contains(&pid) {
            return Next::AlreadyRunning;
        }
        if !readiness.regular && !readiness.login_window {
            return Next::AwaitActivationPolicy;
        }
        if !readiness.finished_launching {
            return Next::AwaitFinishedLaunching;
        }
        self.running.insert(pid);
        Next::Spawn
    }

    /// Every application running when rini started has been looked at. Until then an activation is
    /// left to that first look.
    pub fn discovered(&mut self) {
        self.discovered = true;
    }

    /// Whether an application that has just become frontmost is to be looked at again.
    pub fn look_on_activation(&self, pid: pid_t) -> bool {
        self.discovered && !self.running.contains(&pid)
    }

    /// The thread gave up on the application, or the application quit. The next sighting asks again.
    pub fn ended(&mut self, pid: pid_t) {
        self.running.remove(&pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PID: pid_t = 21114;

    fn seen(regular: bool, finished_launching: bool) -> Readiness {
        Readiness {
            regular,
            finished_launching,
            login_window: false,
        }
    }

    fn discovered() -> AppThreads {
        let mut threads = AppThreads::default();
        threads.discovered();
        threads
    }

    /// Docker Desktop's launch as measured on 2026-10-01, one sighting per row: Carbon at check-in,
    /// the finished-launching observer, the activation-policy observer, then becoming frontmost.
    #[test]
    fn an_application_whose_policy_flips_while_it_launches_is_taken_on_once_it_settles() {
        let mut threads = discovered();

        assert_eq!(
            threads.next(PID, seen(true, false)),
            Next::AwaitFinishedLaunching,
            "Regular at check-in, still launching"
        );
        assert_eq!(
            threads.next(PID, seen(false, true)),
            Next::AwaitActivationPolicy,
            "finished launching, by which time it had hidden its Dock icon"
        );
        assert_eq!(threads.next(PID, seen(true, true)), Next::Spawn, "Regular again");
        assert!(
            !threads.look_on_activation(PID),
            "becoming frontmost afterwards starts nothing"
        );
    }

    /// The backstop: no observer reported the application ready, and becoming frontmost takes it on.
    #[test]
    fn an_application_first_seen_ready_when_it_is_activated_is_taken_on_then() {
        let mut threads = discovered();
        assert_eq!(threads.next(PID, seen(false, true)), Next::AwaitActivationPolicy);

        assert!(threads.look_on_activation(PID));
        assert_eq!(threads.next(PID, seen(true, true)), Next::Spawn);
        assert!(!threads.look_on_activation(PID));
    }

    #[test]
    fn an_activation_before_the_first_look_is_left_to_it() {
        let mut threads = AppThreads::default();
        assert!(!threads.look_on_activation(PID));

        threads.discovered();

        assert!(threads.look_on_activation(PID));
    }

    #[test]
    fn a_launch_reported_twice_does_not_start_a_second_thread() {
        let mut threads = discovered();
        assert_eq!(threads.next(PID, seen(true, true)), Next::Spawn);
        assert_eq!(threads.next(PID, seen(true, true)), Next::AlreadyRunning);
    }

    #[test]
    fn a_thread_that_gave_up_is_replaced_at_the_next_sighting() {
        let mut threads = discovered();
        assert_eq!(threads.next(PID, seen(true, true)), Next::Spawn);

        threads.ended(PID);

        assert!(threads.look_on_activation(PID));
        assert_eq!(threads.next(PID, seen(true, true)), Next::Spawn);
    }

    #[test]
    fn an_application_that_is_not_regular_waits_however_far_its_launch_has_got() {
        let mut threads = discovered();
        assert_eq!(
            threads.next(PID, seen(false, false)),
            Next::AwaitActivationPolicy
        );
        assert_eq!(threads.next(PID, seen(false, true)), Next::AwaitActivationPolicy);
        assert!(threads.look_on_activation(PID), "still unmanaged");
    }

    #[test]
    fn the_login_window_is_taken_on_whatever_its_policy_once_it_has_launched() {
        let mut threads = discovered();
        let login = |finished_launching| Readiness {
            regular: false,
            finished_launching,
            login_window: true,
        };
        assert_eq!(threads.next(627, login(false)), Next::AwaitFinishedLaunching);
        assert_eq!(threads.next(627, login(true)), Next::Spawn);
    }

    #[test]
    fn one_application_s_thread_says_nothing_about_another_s() {
        let mut threads = discovered();
        assert_eq!(threads.next(PID, seen(true, true)), Next::Spawn);
        assert_eq!(threads.next(PID + 1, seen(true, true)), Next::Spawn);

        threads.ended(PID);

        assert!(threads.look_on_activation(PID));
        assert!(!threads.look_on_activation(PID + 1));
    }
}
