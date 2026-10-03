//! Painting for the Devices window: the device table, the driver grants and
//! the refused claims, each given a share of the height so none overdraws
//! another in a short window.

use devinspect::{class_name, reason_name, Denial, Device, Uid};
use xui_app::dashboard as dash;
use xui_app::devinfo::{grants, Grant};
use xui_app::format::uptime;
use xui_core::{Canvas, Color, Point, Rect, Theme};

use crate::State;

/// Gap between sections.
const GAP: i32 = 14;
/// A section's heading plus its column header.
const CHROME: i32 = 28 + dash::ROW;

/// Paint the whole page.
pub(super) fn paint(canvas: &mut dyn Canvas, theme: Theme, state: &State) {
    let subtitle = "device syscall 23 · read-only · 2 s refresh · [r] refresh  [q] quit";
    let content = dash::frame(canvas, theme, "Devices", subtitle);
    let view = &state.view;

    let devices = match &view.devices {
        Ok(devices) => devices,
        Err(code) => {
            unavailable(canvas, theme, content, *code);
            return;
        }
    };
    let grants = match &view.rules {
        Ok(Some(rules)) => grants(rules),
        _ => Vec::new(),
    };
    let denial_rows = match &view.denials {
        Ok(denials) => denials.len().clamp(1, 6),
        Err(_) => 1,
    };

    // Rows each section wants, then fit them into the height top-down; the
    // device table gets whatever the other two leave.
    let footer = 22;
    let fixed = CHROME * 3 + GAP * 2 + footer;
    let grant_rows = grants.len().max(1) as i32 + 1;
    let wanted_tail = (grant_rows + denial_rows as i32) * dash::ROW;
    let room = content.height() - fixed;
    let device_rows = ((room - wanted_tail) / dash::ROW).clamp(1, devices.len().max(1) as i32);
    let tail_rows = ((room - device_rows * dash::ROW) / dash::ROW).max(2);
    let grant_rows = grant_rows.min(tail_rows - 1);
    let denial_rows = (tail_rows - grant_rows).max(1);

    let mut top = content.top;
    top = device_table(canvas, theme, content, top, device_rows, devices) + GAP;
    top = grant_table(canvas, theme, content, top, grant_rows, view, &grants) + GAP;
    denial_table(canvas, theme, content, top, denial_rows, &view.denials);
    footer_line(canvas, theme, content, state, devices);
}

/// A section heading and its column header; returns the first row's top.
fn chrome(
    canvas: &mut dyn Canvas,
    theme: Theme,
    content: Rect,
    top: i32,
    title: &str,
    columns: &[(&str, i32)],
) -> i32 {
    dash::section(
        canvas,
        theme,
        Rect::new(content.left, top, content.right, top + 24),
        title,
    );
    let header = dash::table_header_rect(content, top + 28);
    for (index, (label, left)) in columns.iter().enumerate() {
        let right = columns
            .get(index + 1)
            .map_or(header.right, |next| header.left + next.1 - 8);
        canvas.draw_text(
            label,
            Rect::new(header.left + left, header.top, right, header.bottom),
            &dash::heading(theme.text_secondary, dash::LABEL),
        );
    }
    rule(canvas, theme, header.left, header.right, header.bottom);
    header.bottom
}

fn rule(canvas: &mut dyn Canvas, theme: Theme, left: i32, right: i32, y: i32) {
    canvas.draw_line(Point::new(left, y), Point::new(right, y), theme.border, 1.0);
}

/// One row of cells at the given column offsets.
fn row(canvas: &mut dyn Canvas, content: Rect, y: i32, columns: &[i32], cells: &[(String, Color)]) {
    for (index, (text, color)) in cells.iter().enumerate() {
        let left = content.left + columns[index];
        let right = columns
            .get(index + 1)
            .map_or(content.right, |next| content.left + next - 8);
        dash::cell(
            canvas,
            Rect::new(left, y, right, y + dash::ROW),
            text,
            *color,
            false,
        );
    }
}

const DEVICE_COLUMNS: [i32; 6] = [0, 44, 160, 260, 370, 500];

fn device_table(
    canvas: &mut dyn Canvas,
    theme: Theme,
    content: Rect,
    top: i32,
    rows: i32,
    devices: &[Device],
) -> i32 {
    let claimed = devices
        .iter()
        .filter(|device| device.owner.is_some())
        .count();
    let title = format!("Devices — {} found, {claimed} claimed", devices.len());
    let labels = ["id", "class", "PCI", "vendor:dev", "owner", "rights"];
    let columns: Vec<(&str, i32)> = labels.iter().copied().zip(DEVICE_COLUMNS).collect();
    let mut y = chrome(canvas, theme, content, top, &title, &columns);
    for device in devices.iter().take(rows as usize) {
        let owned = device.owner.is_some();
        let color = if owned {
            theme.text
        } else {
            theme.text_secondary
        };
        let owner = device
            .owner
            .map_or(String::from("free"), |uid| Uid(uid).to_string());
        let cells = [
            (device.id.to_string(), color),
            (device.class_name().to_string(), color),
            (
                format!(
                    "{:02x}/{:02x}/{:02x}",
                    device.class, device.subclass, device.prog_if
                ),
                color,
            ),
            (
                format!("{:04x}:{:04x}", device.vendor, device.device),
                color,
            ),
            (owner, if owned { theme.accent } else { color }),
            (device.rights.to_string(), color),
        ];
        row(canvas, content, y, &DEVICE_COLUMNS, &cells);
        y += dash::ROW;
    }
    if devices.len() > rows as usize {
        more(canvas, theme, content, top, devices.len() - rows as usize);
    }
    y
}

const GRANT_COLUMNS: [i32; 4] = [0, 160, 280, 500];

fn grant_table(
    canvas: &mut dyn Canvas,
    theme: Theme,
    content: Rect,
    top: i32,
    rows: i32,
    view: &xui_app::devinfo::DevView,
    grants: &[Grant],
) -> i32 {
    let title = match &view.rules {
        Ok(Some(rules)) => format!("Driver class rules — {} enforced since boot", rules.len()),
        Ok(None) => String::from("Driver class rules — none installed"),
        Err(code) => format!("Driver class rules — unreadable (errno {code})"),
    };
    let columns = [
        ("driver", 0),
        ("class", 160),
        ("may", 280),
        ("verdict", 500),
    ];
    let mut y = chrome(canvas, theme, content, top, &title, &columns);
    for grant in grants.iter().take((rows - 1).max(0) as usize) {
        let verdict = if grant.allow { "allow" } else { "deny" };
        let cells = [
            (Uid(grant.uid).to_string(), theme.text),
            (class_name(grant.class_id).to_string(), theme.text),
            (grant.methods.join(" "), theme.text),
            (
                verdict.to_string(),
                if grant.allow {
                    theme.accent
                } else {
                    theme.danger
                },
            ),
        ];
        row(canvas, content, y, &GRANT_COLUMNS, &cells);
        y += dash::ROW;
    }
    let note = if matches!(view.rules, Ok(Some(_))) {
        "every other non-root uid is refused every device class; root keeps its authority"
    } else {
        "without class rules, claims are judged by the Messenger policy alone"
    };
    dash::cell(
        canvas,
        Rect::new(content.left, y, content.right, y + dash::ROW),
        note,
        theme.text_secondary,
        false,
    );
    y + dash::ROW
}

const DENIAL_COLUMNS: [i32; 5] = [0, 110, 270, 390, 470];

fn denial_table(
    canvas: &mut dyn Canvas,
    theme: Theme,
    content: Rect,
    top: i32,
    rows: i32,
    denials: &Result<Vec<Denial>, i64>,
) {
    let columns = [
        ("time", 0),
        ("uid", 110),
        ("class", 270),
        ("device", 390),
        ("why", 470),
    ];
    let title = match denials {
        Ok(denials) => format!("Refused claims — {} in the audit ring", denials.len()),
        Err(_) => String::from("Refused claims"),
    };
    let y = chrome(canvas, theme, content, top, &title, &columns);
    let line = |canvas: &mut dyn Canvas, text: &str| {
        dash::cell(
            canvas,
            Rect::new(content.left, y, content.right, y + dash::ROW),
            text,
            theme.text_secondary,
            false,
        );
    };
    let denials = match denials {
        Ok(denials) if denials.is_empty() => return line(canvas, "none: every claim so far was granted"),
        Ok(denials) => denials,
        Err(code) if *code == devinspect::errno::EPERM => {
            return line(canvas, "reading the audit ring needs CAP_AUDIT_READ; desktop sessions run without capabilities")
        }
        Err(code) => return line(canvas, &format!("unreadable (errno {code})")),
    };
    let mut y = y;
    for denial in denials.iter().take(rows as usize) {
        let cells = [
            (uptime(denial.ticks), theme.text),
            (Uid(denial.uid).to_string(), theme.text),
            (class_name(denial.class_id).to_string(), theme.text),
            (denial.device.to_string(), theme.text),
            (reason_name(denial.reason).to_string(), theme.danger),
        ];
        row(canvas, content, y, &DENIAL_COLUMNS, &cells);
        y += dash::ROW;
    }
}

/// "… N more" at the right of a section heading.
fn more(canvas: &mut dyn Canvas, theme: Theme, content: Rect, top: i32, hidden: usize) {
    canvas.draw_text(
        &format!("… {hidden} more (enlarge the window)"),
        Rect::new(content.left, top, content.right, top + 24),
        &dash::heading_end(theme.text_secondary, dash::LABEL),
    );
}

fn footer_line(
    canvas: &mut dyn Canvas,
    theme: Theme,
    content: Rect,
    state: &State,
    devices: &[Device],
) {
    let footer = Rect::new(
        content.left,
        content.bottom - 22,
        content.right,
        content.bottom,
    );
    let drivers = devices
        .iter()
        .filter(|device| device.owner.is_some_and(|uid| uid != 0))
        .count();
    canvas.draw_text(
        &format!(
            "{} refresh(es) · {drivers} device(s) held by unprivileged drivers · devctl shows the same from a shell",
            state.refreshes
        ),
        footer,
        &dash::heading(theme.text_secondary, dash::LABEL),
    );
}

/// The page when the inventory itself cannot be read.
fn unavailable(canvas: &mut dyn Canvas, theme: Theme, content: Rect, code: i64) {
    let card = Rect::new(content.left, content.top, content.right, content.top + 110);
    dash::card(canvas, theme, card);
    canvas.draw_text(
        "Device inventory unavailable",
        Rect::new(
            card.left + 16,
            card.top + 16,
            card.right - 16,
            card.top + 44,
        ),
        &dash::heading(theme.danger, dash::SECTION),
    );
    let why = if code == devinspect::errno::EACCES {
        String::from("the Messenger policy refuses os.kernel.dev to this app")
    } else {
        format!("the device syscall returned errno {code}")
    };
    canvas.draw_text(
        &why,
        Rect::new(
            card.left + 16,
            card.top + 50,
            card.right - 16,
            card.top + 90,
        ),
        &dash::heading(theme.text_secondary, dash::BODY),
    );
}
