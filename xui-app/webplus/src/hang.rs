//! The "this page is not responding" question (issue #862): WebKit pings the page's process, and when
//! six seconds pass without an answer the window offers a way out instead of leaving the last frame
//! up for ever. The model is here (host-tested); the bar is `app/hang.rs`.
//!
//! Wait hides the bar until the page answers and stops answering again (a new episode); Reload ends
//! the stuck process and loads the page again in a fresh one; Stop ends it and leaves the page
//! stopped. Neither of the last two needs the page's cooperation, which is the point. The toolbar's
//! Reload (and F5, the menu) and Stop do the same while the page is stuck, whether or not the bar
//! is up.

/// What the person chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Wait,
    Reload,
    Stop,
}

impl Answer {
    /// The word markers use.
    pub const fn name(self) -> &'static str {
        match self {
            Answer::Wait => "wait",
            Answer::Reload => "reload",
            Answer::Stop => "stop",
        }
    }
}

/// Whether the page answers, and whether the person has dealt with the question.
#[derive(Debug, Default)]
pub struct Watch {
    stuck: bool,
    answered: bool,
}

impl Watch {
    /// The engine reports the page's process answering (`true`) or not (`false`). Returns whether
    /// the bar's visibility changed.
    pub fn responsive(&mut self, responsive: bool) -> bool {
        if self.stuck == !responsive {
            return false; // no change: a Wait stands
        }
        let before = self.visible();
        self.stuck = !responsive;
        // A change starts an episode of its own: a Wait answers this one only.
        self.answered = false;
        before != self.visible()
    }

    /// Whether the page does not answer (the bar may be put away by a Wait): Reload and Stop then
    /// mean the same as the bar's buttons, so a page that was waited for still has a way out.
    pub fn stuck(&self) -> bool {
        self.stuck
    }

    /// Whether the bar is up.
    pub fn visible(&self) -> bool {
        self.stuck && !self.answered
    }

    /// The person answered. Returns `Some(reload)` when the engine must end the process (then the
    /// page loads again, or stays stopped), `None` for Wait.
    pub fn answer(&mut self, answer: Answer) -> Option<bool> {
        if !self.stuck {
            return None;
        }
        self.answered = true;
        match answer {
            Answer::Wait => None,
            Answer::Reload => Some(true),
            Answer::Stop => Some(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_follows_the_page_and_a_wait_lasts_one_episode() {
        let mut watch = Watch::default();
        assert!(!watch.visible());
        assert!(watch.responsive(false), "the bar comes up");
        assert!(watch.visible());
        assert_eq!(watch.answer(Answer::Wait), None);
        assert!(!watch.visible(), "waiting hides it");
        assert!(!watch.responsive(false), "still stuck: nothing changes");
        assert!(!watch.responsive(true), "it was hidden already");
        assert!(watch.responsive(false), "a new episode asks again");
        assert!(watch.responsive(true), "answering again takes it down");
    }

    #[test]
    fn reload_and_stop_end_the_process_and_only_when_the_page_is_stuck() {
        let mut watch = Watch::default();
        assert_eq!(watch.answer(Answer::Reload), None, "nothing to recover");
        watch.responsive(false);
        assert_eq!(watch.answer(Answer::Reload), Some(true));
        assert!(!watch.visible());
        watch.responsive(true);
        watch.responsive(false);
        assert_eq!(watch.answer(Answer::Stop), Some(false));
    }
}
