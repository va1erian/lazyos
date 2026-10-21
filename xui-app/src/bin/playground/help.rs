//! The Playground's Help page: the class-wide developer-tool text and risk
//! table, plus what this app does about each risk
//! (`docs/messenger-playground-plan.md` D7).

use xui_app::devtool::{HelpPage, RiskKind};

pub fn page() -> HelpPage {
    HelpPage::new("Messenger Playground")
        .about(
            "Browse every Messenger interface, read its documentation, and write Rhai scripts \
             against it. Every button on the doc pane writes script text; nothing happens \
             behind your back. Run (F5) runs the buffer, Ctrl+Enter the selection, Stop (Shift+F5) \
             ends a run. Listen shows a topic's events decoded with the type the interface \
             declares in its own window, Topic monitor (Ctrl+T), which also holds the Publish form; Peek runs once and shows its reply in Output here.",
        )
        .about(
            "Drag the two dividers to size the Explorer and the doc pane; a bigger window gives \
             the extra room to the doc pane, the editor and the Output. To read more, hide the \
             editor and the Output with Ctrl+J, the Hide editor button under the doc pane or \
             View > Editor and output; running a script, Insert call, Serve skeleton, Load \
             example or an error line in Output shows them again. Nothing runs because of it, \
             and runs go on while it is hidden. Your layout is remembered.",
        )
        .about(
            "Opening a file, pasting, dropping or loading a Gallery example only fills the \
             editor. Text that did not come from your typing asks once, before its first Run.",
        )
        .does(
            RiskKind::Services,
            "Methods whose documentation says they stop or restart something carry a warning \
             chip; Call asks you to confirm.",
        )
        .does(
            RiskKind::Settings,
            "The system refuses writes to sys/** and /conf; the refusal is shown as the service \
             wrote it.",
        )
        .does(
            RiskKind::Clipboard,
            "Clipboard methods say they read what you copied. Scripts are never run unreviewed.",
        )
        .does(
            RiskKind::Launch,
            "Notifications show this tool as the sender, not the Terminal.",
        )
        .does(
            RiskKind::Publish,
            "System topics are refused by the broker. The Publish form shows the topic you are \
             writing to and nothing else.",
        )
        .does(
            RiskKind::Flood,
            "Rhai's operation limit, Stop per run, a rate cap and a counter on the listener, and \
             a message counter on every run.",
        )
        .does(
            RiskKind::Script,
            "The warning strip (the x at its right end hides it and View > Show developer-tool \
             notice brings it back; this page keeps the warning either way), the review step \
             before the first Run of pasted or loaded text, and no automatic runs.",
        )
        .extra(
            "Call credential services (keyd, accounts)",
            "Nothing: the kernel refuses the call for developer tools",
            "The refusal is shown; use the Terminal. Nothing is kept for these interfaces: no \
             arguments, replies or output.",
        )
        .extra(
            "Serve a name (msg::serve)",
            "Other programs can call the script while its run lasts",
            "The Live tab shows who owns each name; Stop ends the run and frees the name.",
        )
}
