//! `fabricmon`: a windowed Messenger fabric panel.
//!
//! Three read-only syscall-5 views, refreshed on a one-second timer:
//! the global version-2 `FabricStats` snapshot (`stats`), the kernel name
//! registry (`list`, names with owners and interfaces) and the userspace
//! topics broker's `list_topics` reply (topic/subscription counts). `r`
//! refreshes now and `q` quits.
//!
//! Serial evidence: `FABMON:UP:PASS` after the first frame (or
//! `FABMON:UP:FAIL:<errno>` when the fabric snapshot is unreadable),
//! `FABMON:REFRESH:PASS` on `r`, `FABMON:QUIT:PASS` on `q`, and a
//! `FABMON:DATA:...` line with the headline counters.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::dashboard as dash;
use xui_app::fabric::{self, errno_text, FabricStats, RegistryEntry, Topics};
use xui_app::format::{bytes, clip, hex_id};
use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, Event, NodeKind, NodeSpec, PlatformSpec};
use xui_core::{Canvas, Control, Rect, Theme};

/// How often the fabric snapshot refreshes.
const REFRESH_MILLIS: u32 = 1000;

/// One application message.
enum Msg {
    /// The one-second timer fired.
    Tick,
    /// The user pressed `r`.
    Refresh,
    /// The user pressed `q`.
    Quit,
}

/// Everything the panel renders, shared with the painter.
struct State {
    stats: Option<FabricStats>,
    stats_error: Option<i64>,
    registry: Option<Vec<RegistryEntry>>,
    registry_error: Option<i64>,
    topics: Option<Topics>,
    /// The cached broker endpoint; see [`fabric::Broker`].
    broker: fabric::Broker,
    /// Timer ticks since the app started, for the broker poll cadence.
    tick: u64,
    refreshes: u64,
}

/// Every Nth timer tick also polls the topics broker. The broker's bump heap
/// never frees a receive buffer, so a per-second call would exhaust
/// `messengerd` after ~100 polls; the registry and stats are kernel reads and
/// refresh every tick.
const TOPICS_EVERY: u64 = 10;

impl State {
    /// Read every view now.
    fn load() -> State {
        let mut state = State {
            stats: None,
            stats_error: None,
            registry: None,
            registry_error: None,
            topics: None,
            broker: fabric::Broker::new(),
            tick: 0,
            refreshes: 0,
        };
        state.reload(true);
        state
    }

    /// Refresh the fabric snapshot and registry every tick; refresh the topics
    /// broker too on the first load, on `manual` (`r`), and every
    /// [`TOPICS_EVERY`] ticks.
    fn reload(&mut self, manual: bool) {
        match fabric::fabric_stats() {
            Ok(stats) => {
                self.stats = Some(stats);
                self.stats_error = None;
            }
            Err(code) => self.stats_error = Some(code),
        }
        match fabric::registry() {
            Ok(entries) => {
                self.registry = Some(entries);
                self.registry_error = None;
            }
            Err(code) => self.registry_error = Some(code),
        }
        self.tick += 1;
        if manual || self.tick.is_multiple_of(TOPICS_EVERY) {
            self.topics = Some(self.broker.topics());
        }
    }
}

/// The fabric panel app: one owner-drawn node plus its state.
struct Fabricmon {
    state: Rc<RefCell<State>>,
    root: Control<Msg>,
}

impl App for Fabricmon {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick | Msg::Refresh => {
                let mut state = self.state.borrow_mut();
                state.reload(matches!(msg, Msg::Refresh));
                state.refreshes += 1;
                drop(state);
                ui.invalidate(self.root.id());
                if matches!(msg, Msg::Refresh) {
                    println!("FABMON:REFRESH:PASS");
                }
            }
            Msg::Quit => {
                println!("FABMON:QUIT:PASS");
                ui.quit();
            }
        }
    }
}

fn main() {
    let backend = match LazyOSBackend::new() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("FABMON:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    let (width, height) = backend.screen();
    let state = Rc::new(RefCell::new(State::load()));

    {
        let state = Rc::clone(&state);
        backend.on_first_frame(move || {
            let state = state.borrow();
            match &state.stats {
                Some(stats) => {
                    println!("FABMON:UP:PASS");
                    let names = state.registry.as_ref().map_or(0, |entries| entries.len());
                    let topics = match &state.topics {
                        Some(Topics::Online(topics)) => topics.len(),
                        _ => 0,
                    };
                    println!(
                        "FABMON:DATA:channels={} endpoints={} buffers={} fences_submitted={} names={} topics={}",
                        stats.channels,
                        stats.endpoints,
                        stats.buffers,
                        stats.fences_submitted,
                        names,
                        topics
                    );
                }
                None => println!("FABMON:UP:FAIL:{}", state.stats_error.unwrap_or(-22)),
            }
        });
    }

    let spec = PlatformSpec::new("fabricmon")
        .size(xui_core::Dip(width as f32), xui_core::Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, |ui| {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("root node");
        {
            let state = Rc::clone(&state);
            root.set_painter(Rc::new(move |canvas| paint(canvas, &state.borrow())));
        }
        root.on_events(|event| match event {
            Event::Char('r') => Some(Msg::Refresh),
            Event::Char('q') => Some(Msg::Quit),
            _ => None,
        });
        root.focus();
        ui.on_timer(|_| Some(Msg::Tick));
        ui.set_timer(REFRESH_MILLIS);
        Fabricmon { state, root }
    });

    backend.unbind();
    match outcome {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            println!("FABMON:RUN:FAIL:{error}");
            std::process::exit(1);
        }
    }
}

/// Paint the whole panel.
fn paint(canvas: &mut dyn Canvas, state: &State) {
    let theme = Theme::light();
    let bounds = canvas.bounds();
    let subtitle = format!(
        "native syscall 5 · registry list · topics broker · {}×{} · [r] refresh  [q] quit",
        bounds.width(),
        bounds.height()
    );
    let content = dash::frame(canvas, theme, "fabricmon", &subtitle);

    let gap = 16;
    let left_width = content.width() * 52 / 100;
    let left = Rect::new(
        content.left,
        content.top,
        content.left + left_width,
        content.bottom - 30,
    );
    let right = Rect::new(
        left.right + gap,
        content.top,
        content.right,
        content.bottom - 30,
    );

    paint_fabric(canvas, theme, left, state);
    paint_registry(canvas, theme, right, state);
    paint_footer(canvas, theme, content, state);
}

/// The fabric counters card.
fn paint_fabric(canvas: &mut dyn Canvas, theme: Theme, rect: Rect, state: &State) {
    dash::card(canvas, theme, rect);
    dash::section(
        canvas,
        theme,
        Rect::new(rect.left + 16, rect.top + 8, rect.right - 16, rect.top + 32),
        "Fabric — stats ABI v2",
    );

    let Some(stats) = &state.stats else {
        let code = state.stats_error.unwrap_or(-22);
        canvas.draw_text(
            &format!("stats syscall failed: {}", errno_text(code)),
            Rect::new(
                rect.left + 16,
                rect.top + 48,
                rect.right - 16,
                rect.top + 80,
            ),
            &dash::heading(theme.danger, dash::BODY),
        );
        if let Some(code) = state.registry_error {
            canvas.draw_text(
                &format!("registry failed: {}", errno_text(code)),
                Rect::new(
                    rect.left + 16,
                    rect.top + 80,
                    rect.right - 16,
                    rect.top + 112,
                ),
                &dash::heading(theme.text_secondary, dash::LABEL),
            );
        }
        return;
    };

    let yes_no = |value: u64| if value != 0 { "yes" } else { "no" };
    let metrics: Vec<(&str, String)> = vec![
        ("channels", format!("{}", stats.channels)),
        ("endpoints", format!("{}", stats.endpoints)),
        ("queued messages", format!("{}", stats.queued)),
        ("queued bytes", bytes(stats.queued_bytes)),
        ("outstanding txns", format!("{}", stats.outstanding)),
        ("calls", format!("{}", stats.calls)),
        ("replies", format!("{}", stats.replies)),
        ("one-way messages", format!("{}", stats.one_way)),
        ("timeouts", format!("{}", stats.timeouts)),
        ("cancels", format!("{}", stats.cancels)),
        ("drops", format!("{}", stats.drops)),
        ("handles", format!("{}", stats.handles)),
        ("shared buffers", format!("{}", stats.buffers)),
        ("buffer bytes", bytes(stats.buffer_bytes)),
        ("buffer mappings", format!("{}", stats.buffer_mappings)),
        ("zero-copy handoffs", format!("{}", stats.handoffs)),
        ("fences submitted", format!("{}", stats.fences_submitted)),
        ("fence waits", format!("{}", stats.fence_waits)),
        ("fence timeouts", format!("{}", stats.fence_timeouts)),
        (
            "outstanding fences",
            format!("{}", stats.outstanding_fences),
        ),
        ("kernel services", format!("{}", stats.services)),
        ("ACL rules", format!("{}", stats.acl_rules)),
        ("ACL loaded", yes_no(stats.acl_loaded).to_string()),
        ("audit denies", format!("{}", stats.audit_denies)),
        ("audit allows", format!("{}", stats.audit_allows)),
        ("audit events", format!("{}", stats.audit_total)),
    ];

    let columns = 3;
    let column_width = (rect.width() - 32) / columns;
    let rows = metrics.len().div_ceil(columns as usize);
    for (index, (key, value)) in metrics.iter().enumerate() {
        let column = (index / rows) as i32;
        let row = index % rows;
        let left = rect.left + 16 + column * column_width;
        let top = rect.top + 44 + row as i32 * 24;
        dash::key_value(
            canvas,
            theme,
            Rect::new(left, top, left + column_width - 12, top + 22),
            key,
            value,
            theme.text,
        );
    }

    paint_slots(
        canvas,
        theme,
        rect,
        stats,
        rect.top + 44 + rows as i32 * 24 + 14,
    );
}

/// The per-slot usage table: handles, buffers and buffer bytes per live task.
fn paint_slots(canvas: &mut dyn Canvas, theme: Theme, rect: Rect, stats: &FabricStats, top: i32) {
    dash::section(
        canvas,
        theme,
        Rect::new(rect.left + 16, top, rect.right - 16, top + 24),
        "Per-task usage",
    );
    let header = Rect::new(rect.left + 16, top + 26, rect.right - 16, top + 48);
    let columns: [(&str, i32, i32, bool); 4] = [
        ("slot", 0, 60, false),
        ("handles", 60, 140, true),
        ("buffers", 150, 230, true),
        ("buffer bytes", 240, header.width(), true),
    ];
    for (label, left, right, end) in columns {
        let cell = Rect::new(
            header.left + left,
            header.top,
            header.left + right,
            header.bottom,
        );
        let style = if end {
            dash::heading_end(theme.text_secondary, dash::LABEL)
        } else {
            dash::heading(theme.text_secondary, dash::LABEL)
        };
        canvas.draw_text(label, cell, &style);
    }
    canvas.draw_line(
        xui_core::Point::new(header.left, header.bottom),
        xui_core::Point::new(header.right, header.bottom),
        theme.border,
        1.0,
    );

    let mut y = header.bottom;
    let mut shown = 0usize;
    for (slot, usage) in stats.tasks.iter().enumerate() {
        if usage.live == 0 {
            continue;
        }
        if y + dash::ROW > rect.bottom - 12 {
            break;
        }
        dash::cell(
            canvas,
            Rect::new(header.left, y, header.left + 60, y + dash::ROW),
            &format!("{slot}"),
            theme.text,
            false,
        );
        dash::cell(
            canvas,
            Rect::new(header.left + 60, y, header.left + 140, y + dash::ROW),
            &format!("{}", usage.handles),
            theme.text,
            true,
        );
        dash::cell(
            canvas,
            Rect::new(header.left + 150, y, header.left + 230, y + dash::ROW),
            &format!("{}", usage.buffers),
            theme.text,
            true,
        );
        dash::cell(
            canvas,
            Rect::new(header.left + 240, y, header.right, y + dash::ROW),
            &bytes(usage.buffer_bytes),
            theme.text,
            true,
        );
        canvas.draw_line(
            xui_core::Point::new(header.left, y + dash::ROW),
            xui_core::Point::new(header.right, y + dash::ROW),
            theme.border,
            1.0,
        );
        y += dash::ROW;
        shown += 1;
    }
    let live = stats.tasks.iter().filter(|usage| usage.live != 0).count();
    if shown < live {
        canvas.draw_text(
            &format!("+{} more live slot(s)", live - shown),
            Rect::new(header.left, y, header.right, y + dash::ROW),
            &dash::heading(theme.text_secondary, dash::LABEL),
        );
    }
}

/// The name-registry card: one row per name.
fn paint_registry(canvas: &mut dyn Canvas, theme: Theme, rect: Rect, state: &State) {
    let topics_height = 190;
    let registry = Rect::new(
        rect.left,
        rect.top,
        rect.right,
        rect.bottom - topics_height - 16,
    );
    dash::card(canvas, theme, registry);

    let count = state.registry.as_ref().map_or(0, |entries| entries.len());
    dash::section(
        canvas,
        theme,
        Rect::new(
            registry.left + 16,
            registry.top + 8,
            registry.right - 16,
            registry.top + 32,
        ),
        &format!("Registry — {count} name(s)"),
    );

    match &state.registry {
        Some(entries) if !entries.is_empty() => {
            let header = Rect::new(
                registry.left + 16,
                registry.top + 34,
                registry.right - 16,
                registry.top + 58,
            );
            canvas.draw_text(
                "name",
                header,
                &dash::heading(theme.text_secondary, dash::LABEL).bold(),
            );
            let owner = Rect::new(
                header.left + 240,
                header.top,
                header.right - 190,
                header.bottom,
            );
            canvas.draw_text(
                "owner",
                owner,
                &dash::heading(theme.text_secondary, dash::LABEL).bold(),
            );
            let iface = Rect::new(header.right - 180, header.top, header.right, header.bottom);
            canvas.draw_text(
                "interface",
                iface,
                &dash::heading(theme.text_secondary, dash::LABEL).bold(),
            );
            canvas.draw_line(
                xui_core::Point::new(header.left, header.bottom),
                xui_core::Point::new(header.right, header.bottom),
                theme.border,
                1.0,
            );

            let mut y = header.bottom;
            let capacity = ((registry.bottom - 12 - y) / dash::ROW).max(0) as usize;
            for entry in entries.iter().take(capacity) {
                let row = Rect::new(registry.left + 16, y, registry.right - 16, y + dash::ROW);
                dash::cell(
                    canvas,
                    Rect::new(row.left, y, row.left + 240, row.bottom),
                    &clip(&entry.name, 32),
                    theme.text,
                    false,
                );
                dash::cell(
                    canvas,
                    Rect::new(row.left + 240, y, row.right - 190, row.bottom),
                    &format!("slot {}", entry.owner_slot),
                    theme.text_secondary,
                    false,
                );
                let interfaces = if entry.interfaces.is_empty() {
                    "—".to_string()
                } else {
                    let ids: Vec<String> = entry
                        .interfaces
                        .iter()
                        .take(1)
                        .map(|id| hex_id(*id))
                        .collect();
                    ids.join(", ")
                };
                dash::cell(
                    canvas,
                    Rect::new(row.right - 180, y, row.right, row.bottom),
                    &interfaces,
                    theme.text_secondary,
                    false,
                );
                canvas.draw_line(
                    xui_core::Point::new(row.left, row.bottom),
                    xui_core::Point::new(row.right, row.bottom),
                    theme.border,
                    1.0,
                );
                y += dash::ROW;
            }
            if entries.len() > capacity {
                canvas.draw_text(
                    &format!("+{} more name(s)", entries.len() - capacity),
                    Rect::new(registry.left + 16, y, registry.right - 16, y + dash::ROW),
                    &dash::heading(theme.text_secondary, dash::LABEL),
                );
            }
        }
        _ => {
            let code = state.registry_error.unwrap_or(-22);
            canvas.draw_text(
                &format!("registry unavailable: {}", errno_text(code)),
                Rect::new(
                    registry.left + 16,
                    registry.top + 44,
                    registry.right - 16,
                    registry.top + 72,
                ),
                &dash::heading(theme.text_secondary, dash::BODY),
            );
        }
    }

    paint_topics(canvas, theme, rect, state);
}

/// The topics card: broker status and one row per topic.
fn paint_topics(canvas: &mut dyn Canvas, theme: Theme, container: Rect, state: &State) {
    let rect = Rect::new(
        container.left,
        container.bottom - 190,
        container.right,
        container.bottom,
    );
    dash::card(canvas, theme, rect);
    dash::section(
        canvas,
        theme,
        Rect::new(rect.left + 16, rect.top + 8, rect.right - 16, rect.top + 32),
        "Topics — broker",
    );

    match &state.topics {
        Some(Topics::Online(topics)) => {
            canvas.draw_text(
                &format!(
                    "{} topic(s) seen; {} subscription(s)",
                    topics.len(),
                    topics.iter().map(|t| t.subscribers).sum::<u64>()
                ),
                Rect::new(
                    rect.left + 16,
                    rect.top + 32,
                    rect.right - 16,
                    rect.top + 54,
                ),
                &dash::heading(theme.text_secondary, dash::LABEL),
            );
            if topics.is_empty() {
                canvas.draw_text(
                    "no topic has been published since the broker started",
                    Rect::new(
                        rect.left + 16,
                        rect.top + 58,
                        rect.right - 16,
                        rect.top + 84,
                    ),
                    &dash::heading(theme.text_disabled, dash::LABEL),
                );
            }
            let mut y = rect.top + 56;
            for topic in topics.iter().take(4) {
                let row = Rect::new(rect.left + 16, y, rect.right - 16, y + dash::ROW);
                dash::cell(
                    canvas,
                    Rect::new(row.left, y, row.right - 130, row.bottom),
                    &clip(&topic.topic, 34),
                    theme.text,
                    false,
                );
                dash::cell(
                    canvas,
                    Rect::new(row.right - 130, y, row.right - 60, row.bottom),
                    &format!("{} subs", topic.subscribers),
                    theme.text_secondary,
                    true,
                );
                dash::cell(
                    canvas,
                    Rect::new(row.right - 56, y, row.right, row.bottom),
                    if topic.retained { "retained" } else { "—" },
                    theme.text_secondary,
                    false,
                );
                y += dash::ROW;
            }
        }
        Some(Topics::Offline(code)) => {
            canvas.draw_text(
                &format!("broker offline: {}", errno_text(*code)),
                Rect::new(
                    rect.left + 16,
                    rect.top + 44,
                    rect.right - 16,
                    rect.top + 72,
                ),
                &dash::heading(theme.warning, dash::BODY),
            );
            canvas.draw_text(
                "boot the image with a broker (LAZYOS_SERVICES=1 or LAZYOS_MESSENGERD=1) to list topics",
                Rect::new(
                    rect.left + 16,
                    rect.top + 76,
                    rect.right - 16,
                    rect.top + 102,
                ),
                &dash::heading(theme.text_secondary, dash::LABEL),
            );
        }
        None => {}
    }
}

/// The footer: tick and refresh count.
fn paint_footer(canvas: &mut dyn Canvas, theme: Theme, content: Rect, state: &State) {
    let footer = Rect::new(
        content.left,
        content.bottom - 24,
        content.right,
        content.bottom,
    );
    let text = format!(
        "tick {} · {} refresh(es) · stats ABI v2 via syscall 5",
        xui_app::sys::clock_ticks(),
        state.refreshes
    );
    canvas.draw_text(
        &text,
        footer,
        &dash::heading(theme.text_secondary, dash::LABEL),
    );
}
