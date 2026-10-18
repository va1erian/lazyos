"""The scripted sessions the launcher offers (`tools/screenshot/examples/`).

Kept apart from `catalog` to keep it under the file length limit; `catalog`
re-exports `SCRIPTS`.
"""

from __future__ import annotations

# (file, label, switches, xui-app) - `switches` are the image build switches a
# script needs; `xui-app` names the viewer to embed via LAZYOS_XUI_APP. A
# `desktop` script (the document apps) instead boots the desktop profile with
# every desktop app embedded and only `xui-app` autostarted (`term` for the
# scripts that only wait for the Terminal: nothing opens at boot by default).
SCRIPTS = [
    ("type_and_shot.json", "Type & shot (input smoke test)", (), None),
    ("multitask_demo.json", "Multitask (two windows, Tab focus)", (), None),
    ("fs_demo.json", "Filesystem (ls / cat)", (), None),
    ("window_demo.json", "Window move / scroll", (), None),
    ("mouse_demo.json", "Mouse move", (), None),
    ("services_demo.json", "Services (health / log / registry)", ("services",), None),
    ("login_demo.json", "Login & accounts", ("services",), None),
    ("apps_demo.json", "App launch & open-with", ("services",), None),
    ("dnd_drop.json", "Drag & drop (drop)", ("xuid",), None),
    ("dnd_cancel.json", "Drag & drop (cancel)", ("xuid",), None),
    ("xuid_wm.json", "XUID window manager", ("xuid",), None),
    ("xuid_shell.json", "XUID shell protocol (Alt+F4/Tab)", ("xuid", "shellprobe"), None),
    ("xui_counter.json", "XUI app: counter (click to increment)", ("xuid",), "counter"),
    ("xui_sysmon.json", "XUI app: sysmon (tasks, memory, services)", ("xuid", "services"), "sysmon"),
    ("xui_fabricmon.json", "XUI app: fabricmon (services)", ("xuid", "services"), "fabricmon"),
    ("xui_client.json", "XUI app: compositor client (window/focus)", ("xuid", "xui_client"), "client"),
    ("xui_editor.json", "XUI app: Editor (type, save)", ("desktop",), "editor"),
    ("xui_paint.json", "XUI app: Paint (draw, save PNG)", ("desktop",), "paint"),
    ("xui_paint_editor.json", "XUI app: Paint editor (layers, text, gradient, ORA, PNG)",
        ("desktop",), "paint"),
    ("xui_files.json", "XUI app: Files (browse, open)", ("desktop",), "files"),
    ("xui_writer.json", "XUI app: LazyWriter (format, save, export)", ("desktop",), "writer"),
    ("xui_archiver.json", "XUI app: Archiver (open, extract, create, drag to Files)",
     ("desktop",), "archiver"),
    ("xui_settings.json", "XUI app: Settings (menu, colours, layout)", ("desktop",), "term"),
    ("shell_demo.json", "LazyShell (taskbar, start menu, restart)", ("desktop",), "term"),
    ("xui_settings_time.json", "XUI app: Settings (time, clock format, light mode)",
     ("desktop",), "term"),
    ("xui_settings_hidden.json", "XUI app: Settings (hide an app from the start menu)",
     ("desktop",), "term"),
    ("xui_calc.json", "XUI app: Calculator", ("desktop",), "calc"),
    ("xui_pdf.json", "XUI app: PDF Viewer", ("desktop",), "pdf"),
    ("tray.json", "Tray icons (Tray Demo)", ("desktop",), "term"),
    ("lazyrad_pictures.json", "Picture Viewer (open, page, rotate, zoom)", ("desktop",), "term"),
    ("xui_golf.json", "XUI app: LazyGolf", ("desktop",), "golf"),
    ("xui_video.json", "XUI app: Video Player", ("desktop",), "video"),
    ("xui_about.json", "XUI app: About LazyOS", ("desktop",), "about"),
    ("xui_nes.json", "XUI app: NES", ("desktop",), "nes"),
]
