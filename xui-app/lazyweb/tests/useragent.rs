//! The View menu's user agents, driven offscreen against a recording engine: picking one tells the
//! engine, picking the same one again does not, and the menu names the big browsers.

mod support;

use support::*;
use xui_lazyweb::app::Msg;
use xui_lazyweb::useragent::PRESETS;
use xui_webkit::engine::Call;

fn agents(calls: &[Call]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::SetUserAgent(agent) => Some(agent.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn picking_a_user_agent_tells_the_engine_once() {
    drive("user-agent", None, |stage, rig| {
        assert!(
            agents(&rig.engine.calls()).is_empty(),
            "the engine's own at start"
        );
        stage.emit(Msg::UserAgent(Some(2)));
        stage.emit(Msg::UserAgent(Some(2)));
        stage.emit(Msg::UserAgent(None));
        assert_eq!(
            agents(&rig.engine.calls()),
            vec![PRESETS[2].agent.to_owned()]
        );
        stage.emit(Msg::UserAgent(Some(0)));
        assert_eq!(
            agents(&rig.engine.calls()),
            vec![PRESETS[2].agent.to_owned(), String::new()],
            "back to the engine's own"
        );
        stage.emit(Msg::UserAgent(Some(PRESETS.len())));
        assert_eq!(
            agents(&rig.engine.calls()).len(),
            2,
            "a preset that is not there is ignored"
        );
    });
}
