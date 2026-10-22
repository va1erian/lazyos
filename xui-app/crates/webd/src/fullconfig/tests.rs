//! What each scope lets a full view be, and that nothing in a config can widen it.

use super::*;

fn places() -> Places {
    Places {
        profiles: Some(PathBuf::from("/home/user/.apps/os.lazy.webd/profiles")),
        downloads: Some(PathBuf::from("/home/user/Downloads")),
    }
}

fn web() -> wire::ViewConfig {
    wire::ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_WEB,
        ..wire::ViewConfig::default()
    }
}

fn browser() -> wire::ViewConfig {
    wire::ViewConfig {
        profile: BROWSER.into(),
        ..web()
    }
}

fn data_only() -> wire::ViewConfig {
    wire::ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_DATA_ONLY,
        ..wire::ViewConfig::default()
    }
}

fn code(result: Result<Setup, Refusal>) -> i64 {
    result.map(|_| 0).unwrap_or_else(|refusal| refusal.code)
}

#[test]
fn the_view_scope_has_no_network_and_nothing_of_a_browser() {
    let ok = setup(&data_only(), Scope::View, 100, 50, 3, &places()).unwrap();
    let full = ok.engine.full.as_ref().unwrap();
    assert_eq!(ok.engine.group, 3);
    assert_eq!(full.profile, "");
    assert!(full.memory_only, "the app's own profile stays in memory");
    assert_eq!(full.features, FEATURE_POLICY | FEATURE_PAGE_UI);
    assert_eq!(ok.rules.profile, Profile::Own);
    for config in [
        web(),
        wire::ViewConfig {
            scheme_policy: wire::SCHEME_POLICY_ORIGINS,
            origins: vec!["http://a".into()],
            ..wire::ViewConfig::default()
        },
        wire::ViewConfig {
            profile: BROWSER.into(),
            ..data_only()
        },
        wire::ViewConfig {
            handoff: wire::HANDOFF_BROWSER,
            ..data_only()
        },
        wire::ViewConfig {
            downloads: true,
            ..data_only()
        },
        wire::ViewConfig {
            blocking: true,
            ..data_only()
        },
    ] {
        assert_eq!(
            code(setup(&config, Scope::View, 10, 10, 1, &places())),
            EACCES
        );
    }
}

#[test]
fn the_net_scope_loads_the_web_in_the_apps_own_profile_only() {
    let ok = setup(&web(), Scope::Net, 10, 10, 1, &places()).unwrap();
    assert_eq!(ok.rules.policy.schemes, Schemes::Web);
    assert_eq!(
        code(setup(&browser(), Scope::Net, 10, 10, 1, &places())),
        EACCES
    );
    let downloads = wire::ViewConfig {
        downloads: true,
        ..web()
    };
    assert_eq!(
        code(setup(&downloads, Scope::Net, 10, 10, 1, &places())),
        EACCES
    );
}

#[test]
fn the_browser_scope_gets_the_shared_profile_on_disk_and_its_features() {
    let config = wire::ViewConfig {
        downloads: true,
        blocking: true,
        notifications: true,
        dialogs: true,
        deltas: true,
        handoff: wire::HANDOFF_BROWSER,
        user_agent_suffix: "LazyWebPlus/1.0".into(),
        ..browser()
    };
    let ok = setup(&config, Scope::Browser, 800, 600, 2, &places()).unwrap();
    let full = ok.engine.full.as_ref().unwrap();
    assert_eq!(
        Path::new(&full.profile),
        Path::new("/home/user/.apps/os.lazy.webd/profiles/browser")
    );
    assert!(!full.memory_only);
    assert!(full.deltas);
    assert_eq!(
        full.features,
        FEATURE_POLICY
            | FEATURE_PAGE_UI
            | FEATURE_DIALOGS
            | FEATURE_DOWNLOADS
            | FEATURE_NOTIFICATIONS
            | FEATURE_BLOCKING
    );
    assert_eq!(
        ok.rules.downloads.as_deref(),
        Some(Path::new("/home/user/Downloads"))
    );
    // A private window: the same profile's folder (for the rule store), its data in memory.
    let private = wire::ViewConfig {
        ephemeral: true,
        ..browser()
    };
    let ok = setup(&private, Scope::Browser, 10, 10, 2, &places()).unwrap();
    assert!(ok.engine.full.unwrap().memory_only);
    assert_eq!(ok.rules.profile, Profile::Browser { ephemeral: true });
}

#[test]
fn features_that_need_the_browser_profile_are_refused_without_it() {
    for config in [
        wire::ViewConfig {
            downloads: true,
            ..web()
        },
        wire::ViewConfig {
            blocking: true,
            ..web()
        },
        wire::ViewConfig {
            notifications: true,
            ..web()
        },
        wire::ViewConfig {
            notifications: true,
            ephemeral: true,
            ..browser()
        },
    ] {
        assert_eq!(
            code(setup(&config, Scope::Browser, 10, 10, 1, &places())),
            EINVAL
        );
    }
}

#[test]
fn malformed_configs_are_refused() {
    let bad = [
        wire::ViewConfig {
            scheme_policy: 9,
            ..web()
        },
        wire::ViewConfig {
            handoff: 9,
            ..web()
        },
        wire::ViewConfig {
            profile: "other".into(),
            ..web()
        },
        wire::ViewConfig {
            profile: "../browser".into(),
            ..web()
        },
        wire::ViewConfig {
            origins: vec!["not an origin".into()],
            ..web()
        },
        wire::ViewConfig {
            origins: vec!["http://a".into(); ORIGINS_MAX + 1],
            ..web()
        },
        wire::ViewConfig {
            origins: vec![format!("http://{}", "a".repeat(ORIGIN_BYTES_MAX))],
            ..web()
        },
        wire::ViewConfig {
            user_agent_suffix: "x".repeat(USER_AGENT_SUFFIX_MAX + 1),
            ..web()
        },
        wire::ViewConfig {
            user_agent_suffix: "a\nb".into(),
            ..web()
        },
    ];
    for config in bad {
        assert_eq!(
            code(setup(&config, Scope::Browser, 10, 10, 1, &places())),
            EINVAL,
            "{config:?}"
        );
    }
    assert_eq!(code(setup(&web(), Scope::Net, 0, 10, 1, &places())), EINVAL);
}

#[test]
fn origins_are_kept_normalised_and_a_service_without_places_has_no_browser_profile() {
    let config = wire::ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_ORIGINS,
        origins: vec!["HTTP://Example.com:80/path".into()],
        ..wire::ViewConfig::default()
    };
    let ok = setup(&config, Scope::Net, 10, 10, 1, &places()).unwrap();
    assert_eq!(
        ok.rules.policy.origins,
        vec!["http://example.com".to_owned()]
    );
    assert_eq!(
        code(setup(
            &browser(),
            Scope::Browser,
            10,
            10,
            1,
            &Places::default()
        )),
        ENOTSUP
    );
}
