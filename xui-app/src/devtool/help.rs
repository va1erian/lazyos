//! A developer tool's Help page: the class-wide consent text and risk table
//! (owned by `libs/devtool`, so the Installer's consent screen and every Help
//! page say the same words), plus what *this* tool does about each risk.
//!
//! [`HelpPage`] is plain data, tested without a window. [`help_layout`] turns
//! it into a column of xui widgets: wrapped [`FlowText`](xui_core::FlowText)
//! runs and a three-column grid (a `ListView` cell is one clipped line, and
//! these sentences are not), meant to go inside [`help_view`]'s scroll area or
//! a tab page.

use xui_core::arrange::{column, flow_text, grid, label, scroll, Layout, ScrollBuild, Track};
use xui_core::widget::Run;
use xui_core::Dip;

/// What a table cell says for a shared risk the tool gave no text for.
pub const NO_SAFEGUARD: &str = "Nothing beyond the warning strip.";

/// The shared risks, named, in the order of [`devtool::RISKS`]. A tool says
/// what it does about one with [`HelpPage::does`]; a test ties each name to
/// its row, so a reordered or added row cannot silently move a tool's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiskKind {
    /// Stop or restart services, shut the system down.
    Services,
    /// Change or delete the user's own settings.
    Settings,
    /// Read the clipboard.
    Clipboard,
    /// Launch applications and post notifications.
    Launch,
    /// Publish events other programs trust.
    Publish,
    /// Flood the message fabric.
    Flood,
    /// Run a script someone else wrote.
    Script,
}

impl RiskKind {
    /// Every kind, in table order.
    pub const ALL: [RiskKind; 7] = [
        RiskKind::Services,
        RiskKind::Settings,
        RiskKind::Clipboard,
        RiskKind::Launch,
        RiskKind::Publish,
        RiskKind::Flood,
        RiskKind::Script,
    ];

    /// The row of [`devtool::RISKS`] this kind names.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// One row of the Help page's table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpRow {
    /// What a script can do.
    pub what: String,
    /// What follows from it.
    pub consequence: String,
    /// What this tool does about it.
    pub app_does: String,
}

/// The Help page of one developer tool.
#[derive(Clone, Debug)]
pub struct HelpPage {
    app_name: String,
    notes: Vec<String>,
    /// The tool's text per shared risk, in [`devtool::RISKS`] order.
    does: Vec<Option<String>>,
    extra: Vec<HelpRow>,
}

impl HelpPage {
    /// The page of the tool called `app_name` (the window's name without the
    /// "(developer tool)" suffix).
    pub fn new(app_name: &str) -> HelpPage {
        HelpPage {
            app_name: app_name.to_owned(),
            notes: Vec::new(),
            does: vec![None; devtool::RISKS.len()],
            extra: Vec::new(),
        }
    }

    /// Adds a paragraph under the consent text: what the tool is for.
    pub fn about(mut self, paragraph: &str) -> HelpPage {
        self.notes.push(paragraph.to_owned());
        self
    }

    /// Says what the tool does about a shared risk (replacing an earlier
    /// text for the same one).
    pub fn does(mut self, risk: RiskKind, text: &str) -> HelpPage {
        self.does[risk.index()] = Some(text.to_owned());
        self
    }

    /// Adds a risk only this tool has, below the shared ones.
    pub fn extra(mut self, what: &str, consequence: &str, app_does: &str) -> HelpPage {
        self.extra.push(HelpRow {
            what: what.to_owned(),
            consequence: consequence.to_owned(),
            app_does: app_does.to_owned(),
        });
        self
    }

    /// The page's heading.
    pub fn heading(&self) -> String {
        format!("{} Help", crate::devtool::title(&self.app_name))
    }

    /// The first paragraph: what a developer tool is.
    pub fn consent(&self) -> &'static str {
        devtool::CONSENT_TEXT
    }

    /// The tool's own paragraphs.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The table: every shared risk in order (with the tool's text, or
    /// [`NO_SAFEGUARD`]), then the tool's own.
    pub fn rows(&self) -> Vec<HelpRow> {
        let shared = devtool::RISKS.iter().zip(&self.does).map(|(risk, does)| HelpRow {
            what: risk.what.to_owned(),
            consequence: risk.consequence.to_owned(),
            app_does: does.clone().unwrap_or_else(|| NO_SAFEGUARD.to_owned()),
        });
        shared.chain(self.extra.iter().cloned()).collect()
    }

    /// The page as plain text, for a copy or an export.
    pub fn to_text(&self) -> String {
        let mut out = format!("{}\n\n{}\n", self.heading(), self.consent());
        for note in &self.notes {
            out.push_str(&format!("\n{note}\n"));
        }
        out.push_str("\nWhat a script can do\n");
        for row in self.rows() {
            out.push_str(&format!(
                "\n* {}\n  Consequence: {}\n  {}: {}\n",
                row.what, row.consequence, self.app_name, row.app_does
            ));
        }
        out
    }
}

/// The column headings of the table.
const HEADINGS: [&str; 3] = ["What a script can do", "Consequence", "What this tool does"];

/// One wrapped paragraph.
fn paragraph<M: 'static>(text: &str) -> Layout<M> {
    column().child(flow_text().run(Run::normal(text)))
}

/// One wrapped table cell; the first column and the headings are bold.
fn cell<M: 'static>(text: &str, bold: bool) -> xui_core::arrange::Entry<M> {
    let run = if bold { Run::normal(text).weight(700) } else { Run::normal(text) };
    xui_core::arrange::IntoEntry::into_entry(flow_text().run(run))
}

/// The page as a layout: heading, consent text, the tool's paragraphs and the
/// risk table. Put it in [`help_view`] or a scroll area of your own.
pub fn help_layout<M: 'static>(page: &HelpPage) -> Layout<M> {
    let mut cells = Vec::new();
    for heading in HEADINGS {
        cells.push(cell(heading, true));
    }
    for row in page.rows() {
        cells.push(cell(&row.what, true));
        cells.push(cell(&row.consequence, false));
        cells.push(cell(&row.app_does, false));
    }
    let mut body = column()
        .padding(16)
        .gap(12)
        .child(label(page.heading()).title())
        .child(paragraph(page.consent()));
    for note in page.notes() {
        body = body.child(paragraph(note));
    }
    body.child(label("Risks").caption())
        .child(
            grid([Track::Fill(3), Track::Fill(3), Track::Fill(4)])
                .gap(Dip(10.0))
                .children(cells),
        )
}

/// [`help_layout`] in a scrolling area that fills its parent.
pub fn help_view<M: 'static>(page: &HelpPage) -> ScrollBuild<M> {
    scroll(help_layout(page))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word each row of [`devtool::RISKS`] must contain, in `RiskKind` order.
    const KEYWORDS: [&str; 7] = [
        "services", "settings", "clipboard", "applications", "events", "flood", "script",
    ];

    #[test]
    fn every_risk_kind_names_its_row_of_the_shared_table() {
        assert_eq!(RiskKind::ALL.len(), devtool::RISKS.len());
        for (kind, keyword) in RiskKind::ALL.into_iter().zip(KEYWORDS) {
            let risk = &devtool::RISKS[kind.index()];
            assert!(
                risk.what.to_lowercase().contains(keyword),
                "{kind:?} is row {} ({:?})",
                kind.index(),
                risk.what
            );
        }
    }

    #[test]
    fn the_rows_are_the_shared_ones_with_the_tools_text_and_then_its_own() {
        let page = HelpPage::new("Messenger Playground")
            .does(RiskKind::Clipboard, "Never runs unreviewed text.")
            .does(RiskKind::Script, "The review step.")
            .does(RiskKind::Script, "The review step and the strip.")
            .extra("Hold the Run key", "A busy loop", "Stop ends it.");
        let rows = page.rows();
        assert_eq!(rows.len(), devtool::RISKS.len() + 1);
        for (row, risk) in rows.iter().zip(devtool::RISKS) {
            assert_eq!((row.what.as_str(), row.consequence.as_str()), (risk.what, risk.consequence));
        }
        assert_eq!(rows[RiskKind::Clipboard.index()].app_does, "Never runs unreviewed text.");
        assert_eq!(
            rows[RiskKind::Script.index()].app_does,
            "The review step and the strip.",
            "the later text replaces the earlier"
        );
        assert_eq!(rows[RiskKind::Services.index()].app_does, NO_SAFEGUARD);
        assert_eq!(rows.last().unwrap().what, "Hold the Run key");
    }

    #[test]
    fn the_heading_carries_the_suffix_and_the_consent_text_is_the_shared_one() {
        let page = HelpPage::new("Messenger Playground");
        assert_eq!(page.heading(), "Messenger Playground (developer tool) Help");
        assert_eq!(page.consent(), devtool::CONSENT_TEXT);
    }

    #[test]
    fn the_plain_text_holds_every_part() {
        let text = HelpPage::new("Playground")
            .about("Browse and call interfaces.")
            .does(RiskKind::Flood, "Stop ends a run.")
            .to_text();
        assert!(text.contains(devtool::CONSENT_TEXT));
        assert!(text.contains("Browse and call interfaces."));
        for risk in devtool::RISKS {
            assert!(text.contains(risk.what) && text.contains(risk.consequence));
        }
        assert!(text.contains("Playground: Stop ends a run."));
    }
}
