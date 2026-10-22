//! The order of the steps and the marker each one reports under.

/// The steps in the order they run (`Ord`: a stage before `Done` is still running).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Stage {
    View,
    Blocklist,
    Index,
    Blocked,
    Unblocked,
    Page2,
    Back,
    Forward,
    CookieA,
    CookieB,
    CookieA2,
    Download,
    Dialog,
    Popup,
    Held,
    Scope,
    Delta,
    Latency,
    Done,
    Idle,
}

/// The steps that need view A.
pub(super) fn stage_uses_a(stage: Stage) -> bool {
    !matches!(
        stage,
        Stage::View | Stage::CookieB | Stage::CookieA2 | Stage::Scope | Stage::Done | Stage::Idle
    )
}

/// The marker a stage reports under.
pub(super) fn step_name(stage: Stage) -> &'static str {
    match stage {
        Stage::View => "VIEW",
        Stage::Blocklist => "BLOCKLIST",
        Stage::Index | Stage::Page2 => "NAV",
        Stage::Blocked => "BLOCKED",
        Stage::Unblocked => "UNBLOCKED",
        Stage::Back => "BACK",
        Stage::Forward => "FORWARD",
        Stage::CookieA => "COOKIE_A",
        Stage::CookieB => "COOKIE_B",
        Stage::CookieA2 => "COOKIE_A2",
        Stage::Download => "DOWNLOAD",
        Stage::Dialog => "DIALOG",
        Stage::Popup => "POPUP",
        Stage::Held => "HELD",
        Stage::Scope => "SCOPE",
        Stage::Delta => "DELTA",
        Stage::Latency => "LATENCY",
        Stage::Done | Stage::Idle => "DONE",
    }
}

/// The stage after `stage`.
pub(super) fn next_stage(stage: Stage) -> Stage {
    match stage {
        Stage::View => Stage::Blocklist,
        Stage::Blocklist => Stage::Index,
        // A NAV that failed on the index page has said so; the page-two half still runs.
        Stage::Index => Stage::Blocked,
        Stage::Blocked => Stage::Unblocked,
        Stage::Unblocked => Stage::Page2,
        Stage::Page2 => Stage::Back,
        Stage::Back => Stage::Forward,
        Stage::Forward => Stage::CookieA,
        Stage::CookieA => Stage::CookieB,
        Stage::CookieB => Stage::CookieA2,
        Stage::CookieA2 => Stage::Download,
        Stage::Download => Stage::Dialog,
        Stage::Dialog => Stage::Popup,
        Stage::Popup => Stage::Held,
        Stage::Held => Stage::Scope,
        Stage::Scope => Stage::Delta,
        Stage::Delta => Stage::Latency,
        Stage::Latency => Stage::Done,
        Stage::Done | Stage::Idle => Stage::Idle,
    }
}
