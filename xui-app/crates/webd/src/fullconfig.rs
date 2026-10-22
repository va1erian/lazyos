//! A full view's `ViewConfig` (idl/webd.midl), checked against the scope its `CreateView` came
//! through, turned into the engine's configuration and the rules the service and the engine's
//! thread keep for the view (docs/webd-plan.md 3.3 to 3.5).
//!
//! The scope is the interface id of the request, which the kernel checked against the caller's
//! manifest before webd saw it: nothing in the config can name one. What each allows:
//!
//! | | `view.v1` | `net.v1` | `browser.v1` |
//! |---|---|---|---|
//! | scheme policy | `DataOnly` | any | any |
//! | profile | `""` (the app's, in memory) | `""` | `""` or `browser` |
//! | `Handoff.Browser`, downloads, content blocking | no | no | yes (downloads, blocking and notifications also need the `browser` profile) |
//!
//! The app's own profile stays in memory: webd may not name a client's label (the credential gate
//! names a label only to its holder) and label ids last one boot, so there is nothing to key a
//! folder with that a later boot would find again. `browser` is the one profile kept on disk.

use std::path::{Path, PathBuf};

use messenger_generated::os_lazy_webd_view_v1 as wire;

use crate::config::{c_string_safe, check_size, Refusal};
use crate::engine::{
    EngineConfig, FullConfig, FEATURE_BLOCKING, FEATURE_DIALOGS, FEATURE_DOWNLOADS,
    FEATURE_NOTIFICATIONS, FEATURE_PAGE_UI, FEATURE_POLICY,
};
use crate::fabric::errno::{EACCES, EINVAL, ENOTSUP};
use crate::limits::{ORIGINS_MAX, ORIGIN_BYTES_MAX, USER_AGENT_SUFFIX_MAX};
use crate::policy::{origin, Handoff, Policy, Schemes};

/// The scope a `CreateView` came through: the interface of the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// `os.lazy.webd.view.v1`: no network.
    View,
    /// `os.lazy.webd.net.v1`: http and https.
    Net,
    /// `os.lazy.webd.browser.v1`: the `browser` profile, downloads, blocking, hand-offs.
    Browser,
}

/// The profile name a client may ask for besides `""`.
pub const BROWSER: &str = "browser";

/// Which data store a view lives in. Views of one client and one profile share a store and a web
/// process; two profiles never share either.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Profile {
    /// The client's own, in memory.
    Own,
    /// The shared browsing profile: on disk, or in memory (`ephemeral`, a private window).
    Browser { ephemeral: bool },
}

impl Profile {
    /// Its data is on disk.
    pub fn persistent(&self) -> bool {
        matches!(self, Profile::Browser { ephemeral: false })
    }
}

/// Where webd keeps what lasts: the profiles' root (`$HOME/.apps/os.lazy.webd/profiles`) and the
/// person's Downloads folder. A service without them makes no `browser` view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places {
    pub profiles: Option<PathBuf>,
    pub downloads: Option<PathBuf>,
}

impl Places {
    /// The `browser` profile's directory.
    pub fn browser(&self) -> Option<PathBuf> {
        self.profiles.as_ref().map(|root| root.join(BROWSER))
    }
}

/// What the service keeps of a full view's configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rules {
    pub policy: Policy,
    pub profile: Profile,
    pub deltas: bool,
    pub popups: bool,
    pub dialogs: bool,
    pub notifications: bool,
    /// With `notifications`: the app shows the page's notifications (`NotifyShow`), not webd.
    pub notify_in_client: bool,
    pub blocking: bool,
    /// Where downloads go (`downloads` and the browser profile), else none.
    pub downloads: Option<PathBuf>,
}

/// A full view's engine configuration and rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    pub engine: EngineConfig,
    pub rules: Rules,
}

const fn invalid(text: &'static str) -> Refusal {
    Refusal::new(EINVAL, text)
}

const fn denied(text: &'static str) -> Refusal {
    Refusal::new(EACCES, text)
}

fn schemes(value: u32) -> Result<Schemes, Refusal> {
    match value {
        wire::SCHEME_POLICY_WEB => Ok(Schemes::Web),
        wire::SCHEME_POLICY_DATA_ONLY => Ok(Schemes::DataOnly),
        wire::SCHEME_POLICY_ORIGINS => Ok(Schemes::Origins),
        _ => Err(invalid("unknown scheme policy")),
    }
}

fn handoff(value: u32) -> Result<Handoff, Refusal> {
    match value {
        wire::HANDOFF_DENY => Ok(Handoff::Deny),
        wire::HANDOFF_ASK => Ok(Handoff::Ask),
        wire::HANDOFF_BROWSER => Ok(Handoff::Browser),
        _ => Err(invalid("unknown hand-off")),
    }
}

fn origins(list: &[String]) -> Result<Vec<String>, Refusal> {
    if list.len() > ORIGINS_MAX {
        return Err(invalid("too many origins"));
    }
    list.iter()
        .map(|text| {
            if text.len() > ORIGIN_BYTES_MAX {
                return Err(invalid("origin too long"));
            }
            origin(text).ok_or(invalid("an origin is not scheme://host[:port]"))
        })
        .collect()
}

fn path_text(path: &Path) -> Result<String, Refusal> {
    let text = path
        .to_str()
        .filter(|text| c_string_safe(text) && text.starts_with('/'))
        .ok_or(Refusal::new(
            ENOTSUP,
            "the profile folder has no usable path",
        ))?;
    Ok(text.to_owned())
}

/// Check `config` for a view of `width` x `height` made through `scope`, in web process group
/// `group`, and build what the engine and the service need.
pub fn setup(
    config: &wire::ViewConfig,
    scope: Scope,
    width: u32,
    height: u32,
    group: i32,
    places: &Places,
) -> Result<Setup, Refusal> {
    let (width, height) = check_size(width, height)?;
    let schemes = schemes(config.scheme_policy)?;
    let handoff = handoff(config.handoff)?;
    let origins = origins(&config.origins)?;
    if schemes != Schemes::DataOnly && scope < Scope::Net {
        return Err(denied("the network needs os.lazy.webd.net.v1"));
    }
    if handoff == Handoff::Browser && scope < Scope::Browser {
        return Err(denied("a hand-off needs os.lazy.webd.browser.v1"));
    }
    let profile = match config.profile.as_str() {
        "" => Profile::Own,
        BROWSER if scope == Scope::Browser => Profile::Browser {
            ephemeral: config.ephemeral,
        },
        BROWSER => return Err(denied("the browser profile needs os.lazy.webd.browser.v1")),
        _ => return Err(invalid("unknown profile")),
    };
    let browser = matches!(profile, Profile::Browser { .. });
    if (config.downloads || config.blocking) && scope < Scope::Browser {
        return Err(denied(
            "downloads and blocking need os.lazy.webd.browser.v1",
        ));
    }
    if (config.downloads || config.blocking) && !browser {
        return Err(invalid("downloads and blocking need the browser profile"));
    }
    // The rule lists live in the profile's folder, which a private window does not have.
    if config.blocking && !profile.persistent() {
        return Err(invalid(
            "content blocking needs the browser profile kept on disk",
        ));
    }
    // A view with no network carries the light views' lock list, which the blocker's lists would
    // replace at a navigation (the engine refuses the two together); a view with no network has
    // nothing to block anyway.
    if config.blocking && schemes == Schemes::DataOnly {
        return Err(invalid("content blocking needs a network scheme policy"));
    }
    // WebKit refuses a notification permission request in a page whose session is not persistent.
    if config.notifications && !profile.persistent() {
        return Err(invalid(
            "notifications need the browser profile kept on disk",
        ));
    }
    let suffix = &config.user_agent_suffix;
    if suffix.len() > USER_AGENT_SUFFIX_MAX || !suffix.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err(invalid("user agent suffix is not short printable ASCII"));
    }
    // A private window's store is in memory and its group's own: given the profile's folder, the engine
    // would key one memory store by it and keep it for every client's private windows for the session.
    let directory = if profile.persistent() {
        let dir = places
            .browser()
            .ok_or(Refusal::new(ENOTSUP, "webd has no profile folder"))?;
        path_text(&dir)?
    } else {
        String::new()
    };
    let downloads = if config.downloads {
        Some(
            places
                .downloads
                .clone()
                .ok_or(Refusal::new(ENOTSUP, "webd has no Downloads folder"))?,
        )
    } else {
        None
    };
    let mut features = FEATURE_POLICY | FEATURE_PAGE_UI;
    for (wanted, bit) in [
        (config.dialogs, FEATURE_DIALOGS),
        (config.downloads, FEATURE_DOWNLOADS),
        (config.notifications, FEATURE_NOTIFICATIONS),
        (config.blocking, FEATURE_BLOCKING),
    ] {
        if wanted {
            features |= bit;
        }
    }
    let engine = EngineConfig {
        scripts: config.scripts,
        width,
        height,
        group,
        full: Some(FullConfig {
            features,
            profile: directory,
            memory_only: !profile.persistent(),
            deltas: config.deltas,
            user_agent_suffix: suffix.clone(),
            no_network: schemes == Schemes::DataOnly,
        }),
        ..EngineConfig::default()
    };
    let rules = Rules {
        policy: Policy {
            schemes,
            origins,
            handoff,
        },
        profile,
        deltas: config.deltas,
        popups: config.popups,
        dialogs: config.dialogs,
        notifications: config.notifications,
        notify_in_client: config.notifications && config.notify_in_client,
        blocking: config.blocking,
        downloads,
    };
    Ok(Setup { engine, rules })
}

#[cfg(test)]
mod tests;
