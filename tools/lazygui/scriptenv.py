"""Build switches a session script needs beyond its `catalog.SCRIPTS` entry's
`switches` (which the Advanced tab turns into checkboxes): switches with no
checkbox of their own, such as the UI probe that `click_at` resolves against
(issue #538), and apps a normal desktop image does not ship."""

from __future__ import annotations

#: Script file -> the extra `LAZYOS_*` variables its image needs.
SCRIPT_ENV: dict[str, dict[str, str]] = {
    # The tray session (docs/tray-plan.md T1) clicks the Tray Demo's icon by name.
    "tray.json": {"LAZYOS_TRAYDEMO": "1", "LAZYOS_UI_PROBE": "1"},
    # The games are opt-in (LAZYOS_GAMES): the sessions that open them need them in the image.
    "xui_golf.json": {"LAZYOS_GAMES": "1"},
    "xui_nes.json": {"LAZYOS_GAMES": "1"},
    # The Picture Viewer session (docs/lazyrad-pictures.md) clicks its toolbar by name.
    "lazyrad_pictures.json": {"LAZYOS_PICTURES": "1", "LAZYOS_UI_PROBE": "1"},
    # The Paint session (docs/paint-plan.md P1) clicks its tools, swatches and menu bar by name.
    "xui_paint.json": {"LAZYOS_UI_PROBE": "1"},
    # The editing session (P6) names the same controls, plus the layer and dialog ones.
    "xui_paint_editor.json": {"LAZYOS_UI_PROBE": "1"},
}


def script_env(cfg: dict, scripts: list[tuple]) -> dict[str, str]:
    """The extra switches of the script a Scripted session runs, else none."""
    if cfg.get("mode") != "Scripted session":
        return {}
    return dict(SCRIPT_ENV.get(scripts[cfg.get("script", 0)][0], {}))
