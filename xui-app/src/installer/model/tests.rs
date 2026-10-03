use super::*;

fn installed(system_name: &str, version: &str) -> Installed {
    Installed {
        system_name: system_name.to_owned(),
        name: system_name
            .rsplit('.')
            .next()
            .unwrap_or(system_name)
            .to_owned(),
        version: version.to_owned(),
        ..Installed::default()
    }
}

fn package(system_name: &str, problems: &[&str]) -> Package {
    Package {
        name: "Paint".into(),
        system_name: system_name.into(),
        version: "1.0.0".into(),
        problems: problems.iter().map(|p| (*p).to_owned()).collect(),
        ..Package::default()
    }
}

#[test]
fn a_fresh_model_is_an_empty_list() {
    let model = Model::new();
    assert_eq!(model.screen, Screen::List);
    assert!(model.packages.is_empty());
    assert!(!model.list_loaded);
    assert!(model.banner.is_none());
}

#[test]
fn the_wizard_reaches_done_and_returns_to_the_list() {
    let mut model = Model::new();
    model.start_wizard();
    assert_eq!(model.screen, Screen::Choose);
    model.set_path("/tmp/paint.lzp");
    model.inspect_ok("/tmp/paint.lzp".into(), package("org.lazy.paint", &[]));
    assert_eq!(model.screen, Screen::Review);
    assert_eq!(model.inspected_path.as_deref(), Some("/tmp/paint.lzp"));

    assert!(!model.can_install(), "Review is not the consent");
    assert!(model.advance());
    assert_eq!(model.screen, Screen::Permissions);
    assert!(model.can_install());

    model.install_started();
    assert_eq!(model.screen, Screen::Installing);
    assert_eq!(
        model.pending,
        Some(Request::Install("/tmp/paint.lzp".into()))
    );

    let app = installed("org.lazy.paint", "1.0.0");
    model.install_ok(app.clone());
    assert_eq!(model.screen, Screen::Done);
    assert_eq!(model.packages, vec![app.clone()]);
    assert_eq!(model.last_installed.as_ref(), Some(&app));
    assert!(
        model.inspected.is_none(),
        "the package is no longer pending"
    );
    assert!(model.pending.is_none());

    model.done();
    assert_eq!(model.screen, Screen::List);
    assert!(
        model.last_installed.is_none(),
        "Done drops the success data"
    );
    assert_eq!(model.packages, vec![app], "the confirmed app stays listed");
}

#[test]
fn a_package_with_problems_offers_only_close() {
    let mut model = Model::new();
    model.inspect_ok(
        "/tmp/bad.lzp".into(),
        package("org.lazy.bad", &["version \"1\" is not semver"]),
    );
    assert_eq!(model.screen, Screen::Review);
    assert_eq!(model.inspected.as_ref().unwrap().problems.len(), 1);
    assert!(!model.can_advance());
    assert!(
        !model.advance(),
        "a broken package never reaches the consent"
    );
    assert_eq!(model.screen, Screen::Review);
    model.cancel();
    assert_eq!(model.screen, Screen::List);
    assert!(model.inspected.is_none(), "Close drops the package");
}

#[test]
fn inspect_failure_returns_to_the_choose_step_with_the_reason() {
    let mut model = Model::new();
    model.start_wizard();
    model.set_path("/tmp/x.lzp");
    model.inspect_failed("not a zip archive");
    assert_eq!(model.screen, Screen::Choose);
    assert_eq!(model.path_input, "/tmp/x.lzp", "the path can be corrected");
    assert_eq!(model.banner.as_deref(), Some("not a zip archive"));
    assert!(model.inspected.is_none());
}

#[test]
fn inspect_again_drops_the_previous_package_and_pending_request() {
    let mut model = Model::new();
    model.inspect_ok("/tmp/a.lzp".into(), package("org.lazy.a", &[]));
    model.advance();
    model.install_started();
    assert!(model.pending.is_some());
    // A second inspect while the first is pending must not reuse the first.
    model.inspect_ok("/tmp/b.lzp".into(), package("org.lazy.b", &[]));
    assert_eq!(model.inspected.as_ref().unwrap().system_name, "org.lazy.b");
    assert!(model.pending.is_none(), "the stale install was dropped");
    assert_eq!(model.inspected_path.as_deref(), Some("/tmp/b.lzp"));
}

#[test]
fn install_failure_keeps_the_package_and_shows_the_error() {
    let mut model = Model::new();
    model.inspect_ok("/tmp/paint.lzp".into(), package("org.lazy.paint", &[]));
    model.advance();
    model.install_started();
    model.install_failed("pkgd error 13");
    assert_eq!(model.screen, Screen::Permissions);
    assert!(model.can_install(), "the user can retry");
    assert_eq!(model.banner.as_deref(), Some("pkgd error 13"));
    assert!(model.inspected.is_some(), "the user can retry");
    assert!(model.pending.is_none());
    assert!(
        model.packages.is_empty(),
        "nothing is listed before pkgd confirms it"
    );
}

#[test]
fn the_remove_flow_confirms_then_updates_the_list() {
    let mut model = Model::new();
    model.list_loaded(vec![
        installed("org.lazy.a", "1.0.0"),
        installed("org.lazy.b", "2.0.0"),
    ]);
    model.remove_asked(model.packages[0].clone());
    assert_eq!(model.screen, Screen::ConfirmRemove);
    assert_eq!(model.pending_remove_name().as_deref(), Some("org.lazy.a"));
    model.remove_ok("org.lazy.a");
    assert_eq!(model.screen, Screen::List);
    assert_eq!(model.packages.len(), 1);
    assert_eq!(model.packages[0].system_name, "org.lazy.b");
    assert!(model.pending_remove.is_none());
    assert!(model.banner.is_none());
}

#[test]
fn a_failed_remove_keeps_the_app_and_shows_the_error() {
    let mut model = Model::new();
    model.list_loaded(vec![installed("org.lazy.a", "1.0.0")]);
    model.remove_asked(model.packages[0].clone());
    model.remove_failed("permission denied");
    assert_eq!(model.screen, Screen::List);
    assert_eq!(model.packages.len(), 1, "the app is still installed");
    assert_eq!(model.banner.as_deref(), Some("permission denied"));
    assert!(model.pending_remove.is_none());
}

#[test]
fn cancel_does_not_interrupt_a_running_install() {
    let mut model = Model::new();
    model.inspect_ok("/tmp/paint.lzp".into(), package("org.lazy.paint", &[]));
    model.advance();
    model.install_started();
    model.cancel();
    model.back();
    assert_eq!(model.screen, Screen::Installing);
    assert!(model.pending.is_some());
}

#[test]
fn list_failure_keeps_the_previous_rows() {
    let mut model = Model::new();
    model.list_loaded(vec![installed("org.lazy.a", "1.0.0")]);
    model.list_failed("pkgd is unavailable");
    assert_eq!(model.screen, Screen::List);
    assert_eq!(model.packages.len(), 1, "the last good list stays");
    assert_eq!(model.banner.as_deref(), Some("pkgd is unavailable"));
}

#[test]
fn control_characters_never_reach_the_banner() {
    let mut model = Model::new();
    model.inspect_failed("bad\nreason\u{7}");
    assert_eq!(model.banner.as_deref(), Some("badreason"));
}

#[test]
fn a_second_install_of_the_same_id_replaces_the_row() {
    let mut model = Model::new();
    model.list_loaded(vec![installed("org.lazy.paint", "1.0.0")]);
    model.install_ok(installed("org.lazy.paint", "2.0.0"));
    assert_eq!(model.packages.len(), 1);
    assert_eq!(model.packages[0].version, "2.0.0");
}

#[test]
fn back_walks_the_wizard_one_step_at_a_time() {
    let mut model = Model::new();
    model.start_wizard();
    model.inspect_ok("/tmp/paint.lzp".into(), package("org.lazy.paint", &[]));
    model.advance();
    model.banner = Some("pkgd error 13".into());

    model.back();
    assert_eq!(model.screen, Screen::Review);
    assert!(
        model.banner.is_none(),
        "the install error belongs to Permissions"
    );
    assert!(model.inspected.is_some());

    model.back();
    assert_eq!(model.screen, Screen::Choose);
    assert!(
        model.inspected.is_none(),
        "leaving Review drops the package"
    );
    assert!(model.inspected_path.is_none());
    assert_eq!(model.path_input, "/tmp/paint.lzp", "the path is kept");

    model.back();
    assert_eq!(model.screen, Screen::List);
}

#[test]
fn a_package_opened_from_files_starts_at_review_and_backs_into_choose() {
    let mut model = Model::new();
    model.inspect_ok("/home/a.lzp".into(), package("org.lazy.a", &[]));
    assert_eq!(model.screen, Screen::Review);
    model.back();
    assert_eq!(model.screen, Screen::Choose);
    assert_eq!(model.path_input, "/home/a.lzp");
}

#[test]
fn back_is_ignored_outside_the_editable_steps() {
    let mut model = Model::new();
    model.list_loaded(vec![installed("org.lazy.a", "1.0.0")]);
    model.back();
    assert_eq!(model.screen, Screen::List);
    model.remove_asked(model.packages[0].clone());
    model.back();
    assert_eq!(model.screen, Screen::ConfirmRemove);
    model.install_ok(installed("org.lazy.b", "1.0.0"));
    model.back();
    assert_eq!(model.screen, Screen::Done);
}

#[test]
fn a_picked_path_replaces_the_field_and_clears_the_error() {
    let mut model = Model::new();
    model.start_wizard();
    model.inspect_failed("not a zip archive");
    model.path_picked("/DOOM.LZP");
    assert_eq!(model.path_input, "/DOOM.LZP");
    assert!(model.banner.is_none());
    assert_eq!(model.screen, Screen::Choose);
}

#[test]
fn start_wizard_keeps_the_last_path_and_drops_the_banner() {
    let mut model = Model::new();
    model.set_path("/tmp/a.lzp");
    model.list_failed("pkgd is unavailable");
    model.start_wizard();
    assert_eq!(model.screen, Screen::Choose);
    assert_eq!(model.path_input, "/tmp/a.lzp");
    assert!(model.banner.is_none());
}

#[test]
fn install_is_refused_twice_and_outside_permissions() {
    let mut model = Model::new();
    model.inspect_ok("/tmp/a.lzp".into(), package("org.lazy.a", &[]));
    assert!(!model.can_install(), "Review cannot install");
    model.advance();
    model.pending = Some(Request::Install("/tmp/a.lzp".into()));
    assert!(!model.can_install(), "an install is already queued");
}

#[test]
fn every_wizard_screen_has_a_step_and_the_others_none() {
    assert_eq!(Screen::Choose.step(), Some(0));
    assert_eq!(Screen::Review.step(), Some(1));
    assert_eq!(Screen::Permissions.step(), Some(2));
    assert_eq!(Screen::Installing.step(), Some(3));
    assert_eq!(Screen::Done.step(), Some(3));
    assert_eq!(Screen::List.step(), None);
    assert_eq!(Screen::ConfirmRemove.step(), None);
    assert!(Screen::Done.step().unwrap() < WIZARD_STEPS.len());
}
