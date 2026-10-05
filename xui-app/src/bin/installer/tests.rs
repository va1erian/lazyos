//! The installer's screens on the host: each mounted on xui's offscreen
//! backend from a sample model, checked for placement and saved as light and
//! dark screenshots (`target/snapshots/installer-<screen>-{light,dark}.png`).

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::installer::{Installed, Model, Package, Permission, Screen};
use xui_canvas::snapshot::Gallery;
use xui_canvas::OffscreenBackend;
use xui_core::app::{App, Ui};
use xui_core::backend::Backend;
use xui_core::{Rect, Theme};

use super::msg::Msg;
use super::view;
use super::WINDOW;

struct Shown;

impl App for Shown {
    type Msg = Msg;

    fn update(&mut self, _msg: Msg, _ui: &mut Ui<Msg>) {}
}

fn installed(name: &str, core: bool) -> Installed {
    Installed {
        system_name: format!("os.lazy.{}", name.to_lowercase()),
        name: name.to_owned(),
        version: "1.0.0".to_owned(),
        core,
        ..Installed::default()
    }
}

fn package() -> Package {
    Package {
        name: "Package Demo".to_owned(),
        system_name: "org.example.pkgdemo".to_owned(),
        author: "Example".to_owned(),
        version: "0.3.1".to_owned(),
        description: "A sample package".to_owned(),
        install_dir: "org.example.pkgdemo".to_owned(),
        permissions: vec![Permission {
            kind: "net".to_owned(),
            value: "connect".to_owned(),
            risk: "high".to_owned(),
            explanation: "Connects to the network".to_owned(),
        }],
        ..Package::default()
    }
}

/// A model showing `screen`.
fn model(screen: Screen) -> Model {
    let mut model = Model::new();
    model.list_loaded(vec![installed("Paint", false), installed("Files", true)]);
    model.inspected = Some(package());
    model.pending_remove = Some(installed("Paint", false));
    model.screen = screen;
    model
}

#[test]
fn every_screen_lays_out_and_renders_light_and_dark() {
    let screens = [
        ("list", Screen::List),
        ("choose", Screen::Choose),
        ("review", Screen::Review),
        ("permissions", Screen::Permissions),
        ("installing", Screen::Installing),
        ("done", Screen::Done),
        ("confirm", Screen::ConfirmRemove),
    ];
    for (name, screen) in screens {
        let backend = Rc::new(OffscreenBackend::new());
        let client: Rc<RefCell<Rect>> = Rc::default();
        let seen = Rc::clone(&client);
        let capture = Rc::clone(&backend);
        xui_core::app("installer")
            .size(WINDOW.0, WINDOW.1)
            .backend(Rc::clone(&backend) as Rc<dyn Backend>)
            .run(move |ui| {
                let view = view::build(ui, &model(screen)).expect("the screen");
                *seen.borrow_mut() = ui.client_rect();
                let ui = ui.clone();
                capture.set_run_hook(move || {
                    let _view = &view;
                    let gallery = Gallery::parse(None, Some("target/snapshots"));
                    for (variant, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
                        ui.set_theme(theme);
                        let image = ui.capture().expect("a render");
                        gallery
                            .save(&format!("installer-{name}"), variant, &image)
                            .expect("saved");
                    }
                });
                Ok(Shown)
            })
            .expect("the screen ran");
        assert!(
            !client.borrow().is_empty(),
            "{name}: the window has a client area"
        );
    }
}
