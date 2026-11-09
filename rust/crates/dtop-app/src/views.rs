//! Views: pure functions from state and a snapshot to a screen buffer plus mouse hit regions.

use dtop_core::compose::{ComposeProject, ComposeStatus};
use dtop_core::format;
use dtop_core::gpu::GpuDevice;
use dtop_core::logstream::LogStreamKind;
use dtop_core::models::{
    Container, ContainerDetails, ContainerState, DockerImage, DockerNetwork, DockerVolume,
    HealthStatus,
};
use dtop_core::monitoring::ResourceSample;
use dtop_ui::buffer::{Axis, Constraint, Rect, ScreenBuffer};
use dtop_ui::text::{self, Alignment};
use dtop_ui::widgets::{self, ColumnWidth, TableCell, TableColumn, TableLayout, TableStyles};
use dtop_ui::{Attributes, Color, Role, Style, TerminalSize, Theme};

use crate::list::{self, ListItem};
use crate::reducer::HitMap;
use crate::state::*;
use crate::theme_editor::{self, Field, Row, ThemeEditor};

pub struct RenderContext<'a> {
    pub state: &'a AppState,
    pub snapshot: &'a DockerSnapshot,
    pub theme: &'a Theme,
    pub size: TerminalSize,
    /// Pre-formatted wall clock.
    pub clock: String,
    pub now: f64,
    /// Every monitored host, in `state.host_names` order.
    pub hosts: &'a [HostSummary],
}

pub struct RenderedFrame {
    pub buffer: ScreenBuffer,
    pub hit_map: HitMap,
}

/// Screen regions for a size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenLayout {
    pub header: Rect,
    pub metrics: Option<Rect>,
    pub tabs: Rect,
    pub list: Rect,
    pub detail: Option<Rect>,
    pub footer: Rect,
}

pub const MINIMUM_SIZE: TerminalSize = TerminalSize {
    columns: 40,
    rows: 10,
};

impl ScreenLayout {
    pub fn compute(size: TerminalSize, show_details: bool) -> Option<ScreenLayout> {
        if size.columns < MINIMUM_SIZE.columns || size.rows < MINIMUM_SIZE.rows {
            return None;
        }
        let (width, height) = (size.columns, size.rows);
        let header = Rect::new(0, 0, width, 1);
        let footer = Rect::new(0, height - 1, width, 1);
        let metrics_height = if height >= 34 {
            10
        } else if height >= 24 {
            7
        } else {
            0
        };
        let metrics = (metrics_height > 0).then(|| Rect::new(0, 1, width, metrics_height));
        let tabs = Rect::new(0, 1 + metrics_height, width, 1);
        let body = Rect::new(0, tabs.max_y(), width, footer.y - tabs.max_y());
        let mut list = body;
        let mut detail = None;
        if show_details {
            if width >= 120 {
                let parts = body.split(
                    Axis::Horizontal,
                    &[Constraint::Percent(58), Constraint::Fill(1)],
                    0,
                );
                list = parts[0];
                detail = Some(parts[1]);
            } else if body.height >= 18 {
                let parts = body.split(
                    Axis::Vertical,
                    &[Constraint::Percent(55), Constraint::Fill(1)],
                    0,
                );
                list = parts[0];
                detail = Some(parts[1]);
            }
        }
        Some(ScreenLayout {
            header,
            metrics,
            tabs,
            list,
            detail,
            footer,
        })
    }
}

pub fn render(context: &RenderContext<'_>) -> RenderedFrame {
    // While the theme menu is open, everything renders with its draft so edits show live.
    if let Some(editor) = context.state.theme_editor() {
        let draft = RenderContext {
            theme: &editor.draft,
            clock: context.clock.clone(),
            ..*context
        };
        return render_body(&draft);
    }
    render_body(context)
}

fn render_body(context: &RenderContext<'_>) -> RenderedFrame {
    let theme = context.theme;
    let mut buffer = ScreenBuffer::new(context.size.columns, context.size.rows);
    let bounds = buffer.bounds();
    buffer.fill(bounds, ' ', theme.base());
    let mut hit_map = HitMap::new();
    let Some(layout) = ScreenLayout::compute(context.size, context.state.show_details) else {
        let message = format!(
            "Terminal too small ({}x{}); need {}x{}",
            context.size.columns, context.size.rows, MINIMUM_SIZE.columns, MINIMUM_SIZE.rows
        );
        for (offset, line) in text::wrap(&message, context.size.columns.max(1) as usize)
            .iter()
            .enumerate()
        {
            buffer.write(line, 0, offset as i32, theme.style(Role::Warning));
        }
        return RenderedFrame { buffer, hit_map };
    };
    let items = list::items(context.state.tab, context.snapshot, context.state);
    let selected = list::selected_index(&items, context.state, context.state.tab);
    hit_map.host_chips = draw_header(&mut buffer, layout.header, context);
    if let Some(metrics) = layout.metrics {
        draw_metrics(&mut buffer, metrics, context);
    }
    hit_map.tabs = draw_tab_bar(&mut buffer, layout.tabs, context);
    let list_layout = draw_list(&mut buffer, layout.list, &items, selected, context);
    hit_map.rows = list_layout.row_rects.clone();
    hit_map.list_rect = layout.list;
    hit_map.list_visible_rows = list_layout.row_rects.len().max(1);
    hit_map.list_scroll_offset = list_layout.scroll_offset;
    if let Some(detail) = layout.detail {
        hit_map.detail_rect = detail;
        hit_map.detail_panes =
            draw_detail(&mut buffer, detail, selected.map(|i| &items[i]), context);
    }
    draw_footer(&mut buffer, layout.footer, context);
    match &context.state.mode {
        InputMode::Help => draw_help(&mut buffer, context),
        InputMode::Confirm(confirmation) => {
            let (confirm, cancel) = draw_confirmation(&mut buffer, confirmation, context);
            hit_map.confirm_button = confirm;
            hit_map.cancel_button = cancel;
        }
        InputMode::ThemeEditor(editor) => {
            hit_map.editor_rows = draw_theme_editor(&mut buffer, editor)
        }
        InputMode::HostPicker(selected) => {
            hit_map.host_rows = draw_host_picker(&mut buffer, *selected, context)
        }
        _ => {}
    }
    RenderedFrame { buffer, hit_map }
}

// MARK: - Shared styling

pub fn state_role(state: &ContainerState, health: Option<HealthStatus>) -> Role {
    if health == Some(HealthStatus::Unhealthy) {
        return Role::Unhealthy;
    }
    match state {
        ContainerState::Running => Role::StatusRunning,
        ContainerState::Paused => Role::StatusPaused,
        ContainerState::Restarting => Role::StatusRestarting,
        ContainerState::Dead | ContainerState::Removing => Role::StatusDead,
        ContainerState::Created => Role::StatusCreated,
        ContainerState::Exited | ContainerState::Unknown(_) => Role::StatusExited,
    }
}

pub fn compose_role(status: ComposeStatus) -> Role {
    match status {
        ComposeStatus::Running => Role::StatusRunning,
        ComposeStatus::Partial | ComposeStatus::Paused => Role::StatusPaused,
        ComposeStatus::Degraded => Role::StatusDead,
        ComposeStatus::Stopped | ComposeStatus::Empty => Role::StatusExited,
    }
}

pub fn state_symbol(state: &ContainerState) -> &'static str {
    match state {
        ContainerState::Running => "●",
        ContainerState::Paused => "◐",
        ContainerState::Restarting => "↻",
        ContainerState::Created => "○",
        ContainerState::Dead | ContainerState::Removing => "✕",
        ContainerState::Exited | ContainerState::Unknown(_) => "◌",
    }
}

fn on(base: Style, theme: &Theme, role: Role) -> Style {
    base.with_fg(theme.color(role))
}

/// The active tab / host chip: background-colored text on the accent.
fn active_chip(theme: &Theme) -> Style {
    let fg = if theme.color(Role::Background) == Color::Default {
        theme.color(Role::SelectionForeground)
    } else {
        theme.color(Role::Background)
    };
    let mut style = Style::new(fg, theme.color(Role::TabActive)).bold();
    if theme.uses_attributes_for_emphasis {
        style = style.adding(Attributes::REVERSE);
    }
    style
}

// MARK: - Brand mark

type Run = (&'static str, Role);
const COMPACT_MARK: [Run; 5] = [
    (">", Role::Accent),
    (" ", Role::Muted),
    ("▃", Role::Success),
    ("▅", Role::Memory),
    ("▇", Role::Gpu),
];
const LARGE_MARK: [&[Run]; 5] = [
    &[
        ("▀█▄", Role::Accent),
        ("          ", Role::Muted),
        ("██", Role::Gpu),
    ],
    &[
        ("  ▀█▄", Role::Accent),
        ("     ", Role::Muted),
        ("▇▇", Role::Memory),
        (" ", Role::Muted),
        ("██", Role::Gpu),
    ],
    &[
        ("  ▄█▀", Role::Accent),
        ("  ", Role::Muted),
        ("▆▆", Role::Success),
        (" ", Role::Muted),
        ("██", Role::Memory),
        (" ", Role::Muted),
        ("██", Role::Gpu),
    ],
    &[
        ("▄█▀", Role::Accent),
        ("    ", Role::Muted),
        ("██", Role::Success),
        (" ", Role::Muted),
        ("██", Role::Memory),
        (" ", Role::Muted),
        ("██", Role::Gpu),
    ],
    &[("▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔", Role::Muted)],
];
const LARGE_MARK_WIDTH: i32 = 15;

fn draw_runs(
    buffer: &mut ScreenBuffer,
    runs: &[Run],
    x: i32,
    y: i32,
    base: Style,
    theme: &Theme,
    max_width: Option<i32>,
) -> i32 {
    let mut cursor = x;
    for (text, role) in runs {
        let remaining = max_width.map(|m| x + m - cursor);
        if remaining.is_some_and(|r| r <= 0) {
            break;
        }
        // The chevron is bold, like the logo's heavy stroke.
        let mut style = on(base, theme, *role);
        if *role == Role::Accent {
            style = style.bold();
        }
        cursor += buffer.write_clipped(text, cursor, y, style, remaining, None);
    }
    cursor - x
}

// MARK: - Header

fn connection_symbol(connection: &ConnectionState) -> &'static str {
    match connection {
        ConnectionState::Connected => "●",
        ConnectionState::Connecting => "○",
        ConnectionState::Disconnected { .. } => "✕",
    }
}

fn connection_role(connection: &ConnectionState) -> Role {
    match connection {
        ConnectionState::Connected => Role::StatusRunning,
        ConnectionState::Connecting => Role::Warning,
        ConnectionState::Disconnected { .. } => Role::Error,
    }
}

fn draw_header(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    context: &RenderContext<'_>,
) -> Vec<(usize, Rect)> {
    let theme = context.theme;
    let base = Style::new(
        theme.color(Role::HeaderForeground),
        theme.color(Role::HeaderBackground),
    );
    buffer.fill(rect, ' ', base);
    let snapshot = context.snapshot;
    let mut x = rect.x + 1;
    x += draw_runs(
        buffer,
        &COMPACT_MARK,
        x,
        rect.y,
        base,
        theme,
        Some(rect.max_x() - x),
    );
    x += buffer.write_max(
        " dtop ",
        x,
        rect.y,
        on(base, theme, Role::Title).bold(),
        (rect.max_x() - x).max(0),
    );
    let mut hits = Vec::new();
    if context.hosts.len() > 1 {
        // Leave room for the connection summary on the right.
        let limit = rect.max_x() - 30.min(rect.width / 3);
        for (index, host) in context.hosts.iter().enumerate() {
            let label = format!(" {} {} ", connection_symbol(&host.connection), host.name);
            let width = text::width(&label) as i32;
            if x + width > limit {
                x += buffer.write_max(
                    "…",
                    x,
                    rect.y,
                    on(base, theme, Role::Muted),
                    (limit - x).max(0),
                );
                break;
            }
            let style = if index == context.state.active_host {
                active_chip(theme)
            } else {
                on(base, theme, connection_role(&host.connection))
            };
            buffer.write(&label, x, rect.y, style);
            hits.push((index, Rect::new(x, rect.y, width, 1)));
            x += width + 1;
        }
    }
    let mut facts: Vec<String> = Vec::new();
    if let Some(version) = &snapshot.version {
        facts.push(format!("docker {}", version.version));
    }
    if let Some(info) = &snapshot.info {
        if !info.name.is_empty() {
            facts.push(info.name.clone());
        }
        if info.cpu_count > 0 {
            facts.push(format!("{} cpu", info.cpu_count));
        }
        if info.memory_total > 0 {
            facts.push(format::bytes(info.memory_total as f64));
        }
        let gpus = snapshot.metrics.gpu.devices.len();
        if gpus > 0 {
            facts.push(format!("{gpus} gpu"));
        }
    }
    if facts.is_empty() {
        facts.push(snapshot.endpoint.clone());
    }
    let mut right: Vec<(String, Style)> = Vec::new();
    match &snapshot.connection {
        ConnectionState::Connected => {
            right.push((
                format!("● {} up", snapshot.running_count()),
                on(base, theme, Role::StatusRunning),
            ));
            right.push((
                format!("◌ {} down", snapshot.stopped_count()),
                on(base, theme, Role::StatusExited),
            ));
        }
        ConnectionState::Connecting => {
            right.push(("○ connecting".into(), on(base, theme, Role::Warning)))
        }
        ConnectionState::Disconnected { retry_at, .. } => {
            let text = match retry_at {
                Some(at) => format!(
                    "✕ offline · retry {}s",
                    (at - context.now).ceil().max(0.0) as i64
                ),
                None => "✕ offline".into(),
            };
            right.push((text, on(base, theme, Role::Error)));
        }
    }
    if context.state.is_paused {
        right.push(("⏸ paused".into(), on(base, theme, Role::Warning).bold()));
    }
    if !context.clock.is_empty() {
        right.push((context.clock.clone(), on(base, theme, Role::Muted)));
    }
    let right_text = right
        .iter()
        .map(|p| p.0.as_str())
        .collect::<Vec<_>>()
        .join("  ");
    let right_width = text::width(&right_text) as i32 + 1;
    let available = rect.max_x() - x - right_width - 1;
    if available > 4 {
        buffer.write(
            &text::truncate(&facts.join(" · "), available as usize),
            x,
            rect.y,
            on(base, theme, Role::Muted),
        );
    }
    let mut right_x = x.max(rect.max_x() - right_width);
    for (index, (text, style)) in right.iter().enumerate() {
        if index > 0 {
            right_x += buffer.write("  ", right_x, rect.y, base);
        }
        right_x += buffer.write_max(text, right_x, rect.y, *style, rect.max_x() - right_x);
    }
    hits
}

// MARK: - Metrics

#[derive(Clone, Copy)]
enum Panel {
    Cpu,
    Memory,
    Gpu,
    Network,
}

fn draw_metrics(buffer: &mut ScreenBuffer, rect: Rect, context: &RenderContext<'_>) {
    let has_gpu = !context.snapshot.metrics.gpu.devices.is_empty();
    let wide = rect.width >= 90;
    // With a GPU: cpu · mem · gpu · net when wide (≥ 120), cpu · mem · gpu when not.
    let mut panels = vec![Panel::Cpu, Panel::Memory];
    if has_gpu {
        panels.push(Panel::Gpu);
    }
    if wide && (!has_gpu || rect.width >= 120) {
        panels.push(Panel::Network);
    }
    if !has_gpu && !wide {
        panels = vec![Panel::Cpu, Panel::Memory];
    }
    let parts = rect.split(
        Axis::Horizontal,
        &vec![Constraint::Fill(1); panels.len()],
        0,
    );
    for (panel, part) in panels.into_iter().zip(parts) {
        match panel {
            Panel::Cpu => draw_cpu(buffer, part, context),
            Panel::Memory => draw_memory(buffer, part, context),
            Panel::Gpu => draw_gpu(buffer, part, context),
            Panel::Network => draw_network(buffer, part, context),
        }
    }
}

/// "NVIDIA GeForce RTX 3080" → "GeForce RTX 3080": the vendor is implied by the model.
pub fn short_gpu_name(device: &GpuDevice) -> String {
    for prefix in ["NVIDIA ", "AMD ", "Intel(R) ", "Intel ", "Apple "] {
        if let Some(rest) = device.name.strip_prefix(prefix) {
            return if rest.is_empty() {
                device.name.clone()
            } else {
                rest.to_string()
            };
        }
    }
    device.name.clone()
}

fn draw_gpu(buffer: &mut ScreenBuffer, rect: Rect, context: &RenderContext<'_>) {
    let theme = context.theme;
    let gpu = &context.snapshot.metrics.gpu;
    let mut label = gpu
        .average_utilization()
        .map_or_else(|| "n/a".to_string(), |u| format::percent(u, 0));
    if let Some(used) = gpu.total_memory_used() {
        label.push_str(&format!(" · {}", format::bytes_u64(used)));
        if let Some(total) = gpu.total_memory() {
            label.push_str(&format!(" / {}", format::bytes_u64(total)));
        }
    }
    let title = if gpu.devices.len() == 1 {
        "gpu".to_string()
    } else {
        format!("gpu ×{}", gpu.devices.len())
    };
    let inner = widgets::draw_box(
        buffer,
        rect,
        Some(&title),
        Some(&label),
        theme.style(Role::Border),
        Some(theme.style(Role::Gpu).bold()),
        theme.border,
    );
    if inner.is_empty() {
        return;
    }
    let history: Vec<f64> = context
        .snapshot
        .metrics
        .total
        .gpu
        .elements()
        .iter()
        .map(|v| v / 100.0)
        .collect();
    widgets::draw_braille(buffer, inner, &history, 1.0, |f| {
        Style::new(
            Color::blend(
                theme.color(Role::Gpu),
                theme.color(Role::GraphHigh),
                f * 0.7,
            ),
            theme.color(Role::Background),
        )
    });
    let mut note_row = inner.y;
    if inner.height >= 3 {
        let names: Vec<String> = gpu.devices.iter().map(short_gpu_name).collect();
        buffer.write(
            &text::truncate(&names.join(", "), inner.width as usize),
            inner.x,
            note_row,
            theme.style(Role::Muted),
        );
        note_row += 1;
    }
    if let Some(issue) = gpu.issues.first() {
        if note_row < inner.max_y() - 1 {
            buffer.write(
                &text::truncate(issue, inner.width as usize),
                inner.x,
                note_row,
                theme.style(Role::Muted),
            );
        }
    }
}

fn host_cpu_count(context: &RenderContext<'_>) -> i64 {
    context
        .snapshot
        .info
        .as_ref()
        .map_or(context.snapshot.metrics.total_latest.cpu_count, |i| {
            i.cpu_count
        })
        .max(1)
}

fn draw_cpu(buffer: &mut ScreenBuffer, rect: Rect, context: &RenderContext<'_>) {
    let theme = context.theme;
    let cpus = host_cpu_count(context);
    let normalized = context.snapshot.metrics.total_latest.cpu_percent / cpus as f64;
    let label = format!("{} of {cpus} cpu", format::percent(normalized, 1));
    let inner = widgets::draw_box(
        buffer,
        rect,
        Some("cpu"),
        Some(&label),
        theme.style(Role::Border),
        Some(theme.style(Role::Cpu).bold()),
        theme.border,
    );
    let history: Vec<f64> = context
        .snapshot
        .metrics
        .total
        .cpu
        .elements()
        .iter()
        .map(|v| v / (cpus * 100) as f64)
        .collect();
    widgets::draw_braille(buffer, inner, &history, 1.0, |f| {
        Style::new(theme.gradient(f), theme.color(Role::Background))
    });
}

fn draw_memory(buffer: &mut ScreenBuffer, rect: Rect, context: &RenderContext<'_>) {
    let theme = context.theme;
    let used = context.snapshot.metrics.total_latest.memory_usage;
    let capacity = context.snapshot.info.as_ref().map_or(0, |i| i.memory_total);
    let label = if capacity > 0 {
        format!(
            "{} / {}",
            format::bytes_u64(used),
            format::bytes(capacity as f64)
        )
    } else {
        format::bytes_u64(used)
    };
    let inner = widgets::draw_box(
        buffer,
        rect,
        Some("mem"),
        Some(&label),
        theme.style(Role::Border),
        Some(theme.style(Role::Memory).bold()),
        theme.border,
    );
    let history = context.snapshot.metrics.total.memory.elements();
    let maximum = if capacity > 0 {
        capacity as f64
    } else {
        history.iter().copied().fold(1.0, f64::max)
    };
    widgets::draw_braille(buffer, inner, &history, maximum, |f| {
        Style::new(
            Color::blend(
                theme.color(Role::Memory),
                theme.color(Role::GraphHigh),
                f * 0.6,
            ),
            theme.color(Role::Background),
        )
    });
}

fn draw_network(buffer: &mut ScreenBuffer, rect: Rect, context: &RenderContext<'_>) {
    let theme = context.theme;
    let latest = &context.snapshot.metrics.total_latest;
    let label = format!(
        "↓{} ↑{}",
        format::rate(latest.network_rx_rate),
        format::rate(latest.network_tx_rate)
    );
    let inner = widgets::draw_box(
        buffer,
        rect,
        Some("net"),
        Some(&label),
        theme.style(Role::Border),
        Some(theme.style(Role::NetworkRx).bold()),
        theme.border,
    );
    if inner.is_empty() {
        return;
    }
    let halves = inner.split(
        Axis::Vertical,
        &[Constraint::Fill(1), Constraint::Fill(1)],
        0,
    );
    let rx = context.snapshot.metrics.total.network_rx.elements();
    let tx = context.snapshot.metrics.total.network_tx.elements();
    // Shared scale so the halves compare visually.
    let peak = rx.iter().chain(tx.iter()).copied().fold(1024.0, f64::max);
    widgets::draw_braille(buffer, halves[0], &rx, peak, |_| {
        theme.style(Role::NetworkRx)
    });
    widgets::draw_braille(buffer, halves[1], &tx, peak, |_| {
        theme.style(Role::NetworkTx)
    });
}

// MARK: - Tabs and footer

fn draw_tab_bar(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    context: &RenderContext<'_>,
) -> Vec<(Tab, Rect)> {
    let theme = context.theme;
    let snapshot = context.snapshot;
    buffer.fill(rect, ' ', theme.base());
    let mut hits = Vec::new();
    let mut x = rect.x + 1;
    for tab in Tab::ALL {
        let count = match tab {
            Tab::Containers => snapshot.containers.len(),
            Tab::Compose => ComposeProject::group(&snapshot.containers).0.len(),
            Tab::Images => snapshot.images.len(),
            Tab::Networks => snapshot.networks.len(),
            Tab::Volumes => snapshot.volumes.len(),
        };
        let label = format!(" {} {} {count} ", tab.shortcut(), tab.title());
        let width = text::width(&label) as i32;
        if x + width > rect.max_x() {
            break;
        }
        let style = if tab == context.state.tab {
            active_chip(theme)
        } else {
            theme.style(Role::TabInactive)
        };
        buffer.write(&label, x, rect.y, style);
        hits.push((tab, Rect::new(x, rect.y, width, 1)));
        x += width + 1;
    }
    let state = context.state;
    let mut status = Vec::new();
    if matches!(state.tab, Tab::Containers | Tab::Images) {
        status.push(format!(
            "sort {}{}",
            state.sort_key.raw_value(),
            if state.sort_descending { "↓" } else { "↑" }
        ));
    }
    if matches!(state.tab, Tab::Containers | Tab::Compose) {
        status.push(state.status_filter.raw_value().to_string());
    }
    if !state.filter_text.is_empty() {
        status.push(format!("filter \"{}\"", state.filter_text));
    }
    let text = status.join(" · ");
    let width = text::width(&text) as i32;
    if !text.is_empty() && rect.max_x() - 1 - width > x {
        buffer.write(
            &text,
            rect.max_x() - 1 - width,
            rect.y,
            theme.style(Role::Muted),
        );
    }
    hits
}

pub fn footer_hints(context: &RenderContext<'_>) -> Vec<(&'static str, &'static str)> {
    let mut hints = vec![("q", "quit"), ("?", "help"), ("/", "filter")];
    if context.state.has_several_hosts() {
        hints.push(("H", "hosts"));
    }
    let extra: &[(&str, &str)] = match context.state.tab {
        Tab::Containers => &[
            ("s", "sort"),
            ("a", "show"),
            ("u", "start"),
            ("x", "stop"),
            ("r", "restart"),
            ("p", "pause"),
            ("K", "kill"),
            ("D", "remove"),
            ("v", "pane"),
            ("T", "themes"),
        ],
        Tab::Compose => &[
            ("⏎", "fold"),
            ("u", "start"),
            ("U", "up"),
            ("x", "stop"),
            ("r", "restart"),
            ("D", "down"),
            ("v", "pane"),
            ("T", "themes"),
        ],
        Tab::Images | Tab::Networks | Tab::Volumes => &[
            ("D", "remove"),
            ("X", "prune"),
            ("d", "details"),
            ("T", "themes"),
        ],
    };
    hints.extend_from_slice(extra);
    hints
}

fn draw_footer(buffer: &mut ScreenBuffer, rect: Rect, context: &RenderContext<'_>) {
    let theme = context.theme;
    let base = Style::new(
        theme.color(Role::HeaderForeground),
        theme.color(Role::HeaderBackground),
    );
    buffer.fill(rect, ' ', base);
    if context.state.theme_editor().is_some() {
        let hint = " theme menu · changes preview live · s saves to dtop.conf · esc discards";
        buffer.write_max(
            hint,
            rect.x,
            rect.y,
            on(base, theme, Role::Accent),
            rect.width,
        );
        return;
    }
    if context.state.mode == InputMode::Filter {
        let mut x = rect.x + 1;
        x += buffer.write("/", x, rect.y, on(base, theme, Role::Accent).bold());
        x += buffer.write_max(
            &context.state.filter_text,
            x,
            rect.y,
            base,
            rect.max_x() - x - 28,
        );
        buffer.write("▏", x, rect.y, on(base, theme, Role::Accent));
        let hint = "⏎ apply · esc clear";
        buffer.write(
            hint,
            rect.max_x() - text::width(hint) as i32 - 1,
            rect.y,
            on(base, theme, Role::Muted),
        );
        return;
    }
    if let Some(notice) = &context.state.notice {
        let role = match notice.level {
            NoticeLevel::Info => Role::Accent,
            NoticeLevel::Success => Role::Success,
            NoticeLevel::Warning => Role::Warning,
            NoticeLevel::Error => Role::Error,
        };
        buffer.write_max(
            &format!(" {}", notice.text),
            rect.x,
            rect.y,
            on(base, theme, role),
            rect.width,
        );
        return;
    }
    if let ConnectionState::Disconnected { message, .. } = &context.snapshot.connection {
        buffer.write_max(
            &format!(" {message}"),
            rect.x,
            rect.y,
            on(base, theme, Role::Error),
            rect.width,
        );
        return;
    }
    let mut x = rect.x + 1;
    for (key, action) in footer_hints(context) {
        let needed = (text::width(key) + text::width(action) + 3) as i32;
        if x + needed > rect.max_x() {
            break;
        }
        x += buffer.write(key, x, rect.y, on(base, theme, Role::Accent).bold());
        x += buffer.write(
            &format!(" {action}  "),
            x,
            rect.y,
            on(base, theme, Role::Muted),
        );
    }
}

// MARK: - List

fn draw_list(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    items: &[ListItem],
    selected: Option<usize>,
    context: &RenderContext<'_>,
) -> TableLayout {
    let theme = context.theme;
    let tab = context.state.tab;
    let count = if tab == Tab::Compose {
        items
            .iter()
            .filter(|i| i.compose_project().is_some())
            .count()
    } else {
        items.len()
    };
    let title = format!("{} {count}", tab.title().to_lowercase());
    let focused = context.state.mode == InputMode::Normal;
    let border = theme.style(if focused {
        Role::BorderFocused
    } else {
        Role::Border
    });
    let inner = widgets::draw_box(
        buffer,
        rect,
        Some(&title),
        None,
        border,
        Some(theme.style(Role::Title).bold()),
        theme.border,
    );
    if inner.is_empty() {
        return TableLayout::default();
    }
    if items.is_empty() {
        draw_empty_state(buffer, inner, context);
        return TableLayout::default();
    }
    let show_gpu = !context.snapshot.metrics.gpu.devices.is_empty();
    let columns = columns_for(tab, show_gpu);
    let rows: Vec<Vec<TableCell>> = items
        .iter()
        .map(|i| row_for(i, context, show_gpu))
        .collect();
    let styles = TableStyles {
        header: theme.style(Role::TableHeader).bold(),
        row: theme.base(),
        selected: theme.selection().bold(),
        scroll_indicator: theme.style(Role::Muted),
    };
    let offset = context.state.scroll_offsets.get(&tab).copied().unwrap_or(0);
    widgets::draw_table(
        buffer, inner, &columns, &rows, selected, offset, styles, 1, true,
    )
}

fn draw_empty_state(buffer: &mut ScreenBuffer, rect: Rect, context: &RenderContext<'_>) {
    let theme = context.theme;
    let state = context.state;
    let mut role = Role::Muted;
    let lines: Vec<String> = match &context.snapshot.connection {
        ConnectionState::Connecting => vec![format!(
            "Connecting to Docker at {}…",
            context.snapshot.endpoint
        )],
        ConnectionState::Disconnected { message, retry_at } => {
            role = Role::Error;
            let mut lines = text::wrap(message, (rect.width - 4).max(10) as usize);
            if let Some(at) = retry_at {
                lines.push(String::new());
                lines.push(format!(
                    "Retrying in {}s — press R to retry now",
                    (at - context.now).ceil().max(0.0) as i64
                ));
            }
            lines
        }
        ConnectionState::Connected => {
            if !state.filter_text.is_empty() {
                vec![format!(
                    "Nothing matches \"{}\" — press Esc to clear the filter",
                    state.filter_text
                )]
            } else {
                vec![match state.tab {
                    Tab::Containers if state.status_filter == StatusFilter::All => {
                        "No containers yet. Try: docker run -d nginx".to_string()
                    }
                    Tab::Containers => format!(
                        "No {} containers — press a to change",
                        state.status_filter.raw_value()
                    ),
                    Tab::Compose => {
                        "No Compose projects. Start one with: docker compose up -d".into()
                    }
                    Tab::Images => "No images".into(),
                    Tab::Networks => "No networks".into(),
                    Tab::Volumes => "No volumes".into(),
                }]
            }
        }
    };
    // Before dtop has reached Docker, show the logo above the status when there is room.
    let splash_height = LARGE_MARK.len() as i32 + 3;
    let splash = !context.snapshot.connection.is_connected()
        && rect.height >= lines.len() as i32 + splash_height + 2
        && rect.width >= LARGE_MARK_WIDTH + 4;
    let mut top = rect.y
        + ((rect.height - lines.len() as i32 - if splash { splash_height } else { 0 }) / 2).max(0);
    if splash {
        let x = rect.x + (rect.width - LARGE_MARK_WIDTH) / 2;
        for (offset, row) in LARGE_MARK.iter().enumerate() {
            let y = top + offset as i32;
            if y >= rect.y && y < rect.max_y() && x < rect.max_x() {
                draw_runs(
                    buffer,
                    row,
                    x,
                    y,
                    theme.base(),
                    theme,
                    Some(rect.max_x() - x),
                );
            }
        }
        let name = format!("dtop {APP_VERSION}");
        let name_x = rect.x + ((rect.width - text::width(&name) as i32) / 2).max(0);
        buffer.write_max(
            &name,
            name_x,
            top + LARGE_MARK.len() as i32 + 1,
            theme.style(Role::Title).bold(),
            rect.width,
        );
        top += splash_height;
    }
    for (offset, line) in lines.iter().enumerate() {
        let y = top + offset as i32;
        if y >= rect.max_y() {
            break;
        }
        let text = text::truncate(line, rect.width as usize);
        let x = rect.x + ((rect.width - text::width(&text) as i32) / 2).max(0);
        buffer.write(&text, x, y, theme.style(role));
    }
}

fn col(title: &str, width: ColumnWidth, alignment: Alignment, priority: i32) -> TableColumn {
    TableColumn::new(title, width, alignment, priority)
}

/// Inserts the GPU column after MEM% only when the host has a GPU.
fn with_gpu<T>(show: bool, mut items: Vec<T>, gpu: T) -> Vec<T> {
    if show {
        let at = items.len().min(6);
        items.insert(at, gpu);
    }
    items
}

pub fn columns_for(tab: Tab, show_gpu: bool) -> Vec<TableColumn> {
    use Alignment::{Left, Right};
    use ColumnWidth::{Fixed, Flexible};
    match tab {
        Tab::Containers | Tab::Compose => with_gpu(
            show_gpu,
            vec![
                col("", Fixed(1), Left, 0),
                col("NAME", Flexible { min: 14, weight: 3 }, Left, 0),
                col("STATE", Fixed(10), Left, 1),
                col("CPU%", Fixed(6), Right, 0),
                col("MEM", Fixed(7), Right, 0),
                col("MEM%", Fixed(5), Right, 3),
                col("NET ↓↑/s", Fixed(11), Right, 4),
                col(
                    if tab == Tab::Compose {
                        "SERVICE"
                    } else {
                        "IMAGE"
                    },
                    Flexible { min: 12, weight: 2 },
                    Left,
                    2,
                ),
                col("PORTS", Flexible { min: 12, weight: 1 }, Left, 6),
                col("STATUS", Flexible { min: 14, weight: 2 }, Left, 5),
            ],
            col("GPU%", Fixed(5), Right, 3),
        ),
        Tab::Images => vec![
            col("REPOSITORY:TAG", Flexible { min: 20, weight: 3 }, Left, 0),
            col("ID", Fixed(12), Left, 2),
            col("SIZE", Fixed(8), Right, 0),
            col("CREATED", Fixed(10), Right, 1),
            col("USED", Fixed(4), Right, 3),
        ],
        Tab::Networks => vec![
            col("NAME", Flexible { min: 16, weight: 2 }, Left, 0),
            col("ID", Fixed(12), Left, 3),
            col("DRIVER", Fixed(8), Left, 0),
            col("SCOPE", Fixed(6), Left, 4),
            col("SUBNET", Flexible { min: 18, weight: 1 }, Left, 1),
            col("CTRS", Fixed(4), Right, 2),
        ],
        Tab::Volumes => vec![
            col("NAME", Flexible { min: 18, weight: 3 }, Left, 0),
            col("DRIVER", Fixed(8), Left, 2),
            col("PROJECT", Flexible { min: 10, weight: 1 }, Left, 1),
            col("MOUNTPOINT", Flexible { min: 20, weight: 2 }, Left, 3),
        ],
    }
}

pub fn network_cell(rx: f64, tx: f64) -> String {
    format!(
        "{} {}",
        format::compact_bytes(rx),
        format::compact_bytes(tx)
    )
}

/// One decimal below 10% so small-but-nonzero usage does not read as 0.
pub fn percent_cell(value: f64) -> String {
    format::fixed(value, if value < 10.0 { 1 } else { 0 })
}

fn gpu_cell(sample: Option<&ResourceSample>, active: bool, theme: &Theme) -> TableCell {
    match sample.and_then(|s| s.gpu_percent).filter(|_| active) {
        Some(percent) => TableCell::styled(
            percent_cell(percent),
            Style::fg(theme.gradient(percent / 100.0)),
        ),
        None => TableCell::styled("-", theme.style(Role::Muted)),
    }
}

pub fn row_for(item: &ListItem, context: &RenderContext<'_>, show_gpu: bool) -> Vec<TableCell> {
    let theme = context.theme;
    let metrics = &context.snapshot.metrics;
    let muted = theme.style(Role::Muted);
    match item {
        ListItem::Container {
            container,
            depth,
            is_last,
        } => {
            let state_style = theme.style(state_role(&container.state, container.health()));
            let sample = metrics.latest.get(&container.id);
            let active = container.state.is_active() && sample.is_some();
            let name = if *depth > 0 {
                format!(
                    "{}{}",
                    if *is_last { "└─ " } else { "├─ " },
                    container.name()
                )
            } else {
                container.name()
            };
            let state_text = match container.health() {
                Some(HealthStatus::Healthy) => format!("{} ✓", container.state.raw_value()),
                Some(HealthStatus::Unhealthy) => "unhealthy".into(),
                Some(HealthStatus::Starting) => "starting".into(),
                _ => container.state.raw_value().to_string(),
            };
            let live = sample.filter(|_| active);
            let cpu_fraction = sample.map_or(0.0, ResourceSample::normalized_cpu_percent) / 100.0;
            let memory_fraction = sample.map_or(0.0, ResourceSample::memory_percent) / 100.0;
            with_gpu(
                show_gpu,
                vec![
                    TableCell::styled(state_symbol(&container.state), state_style),
                    if *depth > 0 {
                        TableCell::new(name)
                    } else {
                        TableCell::styled(name, Style::PLAIN.bold())
                    },
                    TableCell::styled(state_text, state_style),
                    match live {
                        Some(s) => TableCell::styled(
                            format::fixed(s.cpu_percent, 1),
                            Style::fg(theme.gradient(cpu_fraction)),
                        ),
                        None => TableCell::styled("-", muted),
                    },
                    match live {
                        Some(s) => TableCell::new(format::bytes_u64(s.memory_usage)),
                        None => TableCell::styled("-", muted),
                    },
                    match live {
                        Some(s) if s.memory_limit > 0 => TableCell::styled(
                            percent_cell(s.memory_percent()),
                            Style::fg(theme.gradient(memory_fraction)),
                        ),
                        Some(_) => {
                            TableCell::styled("-", Style::fg(theme.gradient(memory_fraction)))
                        }
                        None => TableCell::styled("-", muted),
                    },
                    TableCell::styled(
                        live.map_or_else(
                            || "-".into(),
                            |s| network_cell(s.network_rx_rate, s.network_tx_rate),
                        ),
                        muted,
                    ),
                    TableCell::styled(
                        if *depth > 0 {
                            container
                                .compose_service()
                                .unwrap_or(&container.image)
                                .to_string()
                        } else {
                            container.image.clone()
                        },
                        muted,
                    ),
                    TableCell::styled(container.port_summary(), muted),
                    TableCell::styled(container.status.clone(), muted),
                ],
                gpu_cell(sample, active, theme),
            )
        }
        ListItem::ComposeProject { project, collapsed } => {
            let role = compose_role(project.aggregate_status());
            let (mut cpu, mut memory, mut rx, mut tx, mut gpu) = (0.0, 0u64, 0.0, 0.0, None::<f64>);
            for container in project.containers() {
                let Some(s) = metrics.latest.get(&container.id) else {
                    continue;
                };
                cpu += s.cpu_percent;
                memory = memory.wrapping_add(s.memory_usage);
                rx += s.network_rx_rate;
                tx += s.network_tx_rate;
                if let Some(share) = s.gpu_percent {
                    gpu = Some(gpu.unwrap_or(0.0) + share);
                }
            }
            let services = project
                .services
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let total = project.containers().count();
            with_gpu(
                show_gpu,
                vec![
                    TableCell::styled(
                        if *collapsed { "▸" } else { "▾" },
                        theme.style(Role::Accent),
                    ),
                    TableCell::styled(project.name.clone(), theme.style(Role::Accent).bold()),
                    TableCell::styled(
                        format!("{}/{total} up", project.running_count()),
                        theme.style(role),
                    ),
                    TableCell::new(format::fixed(cpu, 1)),
                    TableCell::new(format::bytes_u64(memory)),
                    TableCell::new(""),
                    TableCell::styled(network_cell(rx, tx), muted),
                    TableCell::styled(services, muted),
                    TableCell::new(""),
                    TableCell::styled(project.aggregate_status().raw_value(), theme.style(role)),
                ],
                TableCell::styled(
                    gpu.map_or_else(String::new, |g| percent_cell(g.min(100.0))),
                    theme.style(Role::Gpu),
                ),
            )
        }
        ListItem::Image(image) => vec![
            if image.is_dangling() {
                TableCell::styled(image.display_name(), muted)
            } else {
                TableCell::new(image.display_name())
            },
            TableCell::styled(image.short_id(), muted),
            TableCell::new(format::bytes(image.size as f64)),
            TableCell::styled(format::age(image.created as f64, context.now), muted),
            TableCell::styled(
                if image.containers >= 0 {
                    image.containers.to_string()
                } else {
                    "-".into()
                },
                muted,
            ),
        ],
        ListItem::Network(network) => {
            let subnets = network
                .ipam
                .iter()
                .map(|c| c.subnet.as_str())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            vec![
                if network.is_built_in() {
                    TableCell::styled(network.name.clone(), muted)
                } else {
                    TableCell::new(network.name.clone())
                },
                TableCell::styled(network.short_id(), muted),
                TableCell::new(network.driver.clone()),
                TableCell::styled(network.scope.clone(), muted),
                TableCell::styled(subnets, muted),
                TableCell::new(network.container_ids.len().to_string()),
            ]
        }
        ListItem::Volume(volume) => vec![
            if volume.is_anonymous() {
                TableCell::styled(format!("{}… (anonymous)", &volume.name[..12]), muted)
            } else {
                TableCell::new(volume.name.clone())
            },
            TableCell::styled(volume.driver.clone(), muted),
            TableCell::styled(
                volume.compose_project().unwrap_or("").to_string(),
                theme.style(Role::Accent),
            ),
            TableCell::styled(volume.mountpoint.clone(), muted),
        ],
    }
}

// MARK: - Details

type Line = (String, String, Option<Role>);

fn line(key: &str, value: impl Into<String>, role: Option<Role>) -> Line {
    (key.to_string(), value.into(), role)
}

fn draw_detail(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    item: Option<&ListItem>,
    context: &RenderContext<'_>,
) -> Vec<(DetailPane, Rect)> {
    let theme = context.theme;
    let Some(item) = item else {
        let inner = widgets::draw_box(
            buffer,
            rect,
            Some("details"),
            None,
            theme.style(Role::Border),
            Some(theme.style(Role::Title)),
            theme.border,
        );
        if !inner.is_empty() {
            buffer.write_max(
                "Select an item to see details",
                inner.x + 1,
                inner.y,
                theme.style(Role::Muted),
                inner.width - 1,
            );
        }
        return Vec::new();
    };
    match item {
        ListItem::Container { container, .. } => {
            return draw_container(buffer, rect, container, context)
        }
        ListItem::ComposeProject { project, .. } => draw_lines(
            buffer,
            rect,
            &project.name,
            &project_lines(project),
            context,
        ),
        ListItem::Image(image) => draw_lines(
            buffer,
            rect,
            &image.display_name(),
            &image_lines(image, context),
            context,
        ),
        ListItem::Network(network) => draw_lines(
            buffer,
            rect,
            &network.name,
            &network_lines(network, context),
            context,
        ),
        ListItem::Volume(volume) => {
            draw_lines(buffer, rect, &volume.name, &volume_lines(volume), context)
        }
    }
    Vec::new()
}

fn draw_container(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    container: &Container,
    context: &RenderContext<'_>,
) -> Vec<(DetailPane, Rect)> {
    let theme = context.theme;
    let title_style = theme
        .style(state_role(&container.state, container.health()))
        .bold();
    let inner = widgets::draw_box(
        buffer,
        rect,
        Some(&container.name()),
        None,
        theme.style(Role::Border),
        Some(title_style),
        theme.border,
    );
    if inner.height < 2 {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let mut x = inner.x;
    for pane in DetailPane::ALL {
        let label = format!(" {} ", pane.title());
        let width = text::width(&label) as i32;
        if x + width > inner.max_x() {
            break;
        }
        let style = if pane == context.state.detail_pane {
            theme.selection().bold()
        } else {
            theme.style(Role::Muted)
        };
        buffer.write(&label, x, inner.y, style);
        hits.push((pane, Rect::new(x, inner.y, width, 1)));
        x += width + 1;
    }
    let detail = context
        .snapshot
        .detail
        .as_ref()
        .filter(|d| d.container_id == container.id);
    if context.state.detail_pane == DetailPane::Logs {
        if let Some((text, role)) = log_status(detail, context.state) {
            let width = text::width(text) as i32;
            if inner.max_x() - width > x {
                buffer.write(text, inner.max_x() - width, inner.y, theme.style(role));
            }
        }
    }
    let content = inner.inset_edges(1, 0, 0, 0);
    if content.is_empty() {
        return hits;
    }
    if let Some(error) = detail.and_then(|d| d.error.as_ref()) {
        buffer.write(
            &text::truncate(error, content.width as usize),
            content.x,
            content.y,
            theme.style(Role::Error),
        );
        return hits;
    }
    match context.state.detail_pane {
        DetailPane::Overview => draw_overview(buffer, content, container, detail, context),
        DetailPane::Processes => draw_processes(buffer, content, container, detail, context),
        DetailPane::Logs => draw_logs(buffer, content, detail, context),
    }
    hits
}

pub fn container_lines(
    container: &Container,
    details: Option<&ContainerDetails>,
    sample: Option<&ResourceSample>,
) -> Vec<(String, String, Option<Role>)> {
    let mut lines = vec![
        line("id", container.short_id(), None),
        line("image", container.image.clone(), None),
        line(
            "state",
            if container.status.is_empty() {
                container.state.raw_value().to_string()
            } else {
                container.status.clone()
            },
            Some(state_role(&container.state, container.health())),
        ),
    ];
    if let Some(project) = container.compose_project() {
        lines.push(line(
            "compose",
            format!("{project} / {}", container.compose_service().unwrap_or("?")),
            Some(Role::Accent),
        ));
    }
    if let Some(s) = sample.filter(|_| container.state.is_active()) {
        lines.push(line("cpu", format::percent(s.cpu_percent, 1), None));
        let limit = if s.memory_limit > 0 {
            format!(" / {}", format::bytes_u64(s.memory_limit))
        } else {
            String::new()
        };
        lines.push(line(
            "memory",
            format!("{}{limit}", format::bytes_u64(s.memory_usage)),
            None,
        ));
        lines.push(line(
            "net",
            format!(
                "↓{} ↑{}",
                format::rate(s.network_rx_rate),
                format::rate(s.network_tx_rate)
            ),
            None,
        ));
        lines.push(line(
            "disk",
            format!(
                "r {} w {}",
                format::rate(s.block_read_rate),
                format::rate(s.block_write_rate)
            ),
            None,
        ));
        lines.push(line("pids", s.pids.to_string(), None));
        if let Some(gpu) = s.gpu_percent {
            let memory = s
                .gpu_memory
                .map_or_else(String::new, |m| format!(" · {}", format::bytes_u64(m)));
            lines.push(line(
                "gpu",
                format!("{}{memory}", format::percent(gpu, 1)),
                Some(Role::Gpu),
            ));
        }
    }
    if let Some(d) = details {
        if !d.command_line().is_empty() {
            lines.push(line("command", d.command_line(), None));
        }
        for access in &d.gpu_access {
            lines.push(line("gpu access", access.clone(), Some(Role::Gpu)));
        }
        if d.restart_count > 0 || !d.restart_policy.is_empty() {
            let policy = if d.restart_policy.is_empty() {
                "no"
            } else {
                &d.restart_policy
            };
            lines.push(line(
                "restarts",
                format!("{} (policy {policy})", d.restart_count),
                (d.restart_count > 0).then_some(Role::Warning),
            ));
        }
        if d.state.oom_killed {
            lines.push(line("oom", "killed by the OOM killer", Some(Role::Error)));
        }
        if !d.state.running && d.state.exit_code != 0 {
            lines.push(line(
                "exit code",
                d.state.exit_code.to_string(),
                Some(Role::Error),
            ));
        }
        if !d.state.error.is_empty() {
            lines.push(line("error", d.state.error.clone(), Some(Role::Error)));
        }
        if let Some(health) = d.state.health.filter(|h| *h != HealthStatus::None) {
            let streak = if d.state.failing_streak > 0 {
                format!(" ({} failing)", d.state.failing_streak)
            } else {
                String::new()
            };
            let role = if health == HealthStatus::Healthy {
                Role::Healthy
            } else {
                Role::Unhealthy
            };
            lines.push(line(
                "health",
                format!("{}{streak}", health.raw_value()),
                Some(role),
            ));
        }
        for network in &d.networks {
            let value = if network.ip_address.is_empty() {
                network.name.clone()
            } else {
                format!("{} {}", network.name, network.ip_address)
            };
            lines.push(line("network", value, None));
        }
        for (port, bindings) in &d.port_bindings {
            if !bindings.is_empty() {
                lines.push(line(
                    "port",
                    format!("{} → {port}", bindings.join(", ")),
                    None,
                ));
            }
        }
        for mount in &d.mounts {
            let source = if mount.kind == "volume" {
                mount.name.clone().unwrap_or_else(|| mount.source.clone())
            } else {
                mount.source.clone()
            };
            lines.push(line(
                "mount",
                format!(
                    "{source} → {}{}",
                    mount.destination,
                    if mount.read_write { "" } else { " (ro)" }
                ),
                None,
            ));
        }
    } else if !container.ports.is_empty() {
        lines.push(line("ports", container.port_summary(), None));
    }
    lines
}

fn draw_overview(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    container: &Container,
    detail: Option<&ContainerDetailData>,
    context: &RenderContext<'_>,
) {
    let theme = context.theme;
    let mut area = rect;
    if let Some(series) = context
        .snapshot
        .metrics
        .series
        .get(&container.id)
        .filter(|_| container.state.is_active() && area.height >= 8)
    {
        let width = (area.width - 5).max(1) as usize;
        buffer.write("cpu ", area.x, area.y, theme.style(Role::Muted));
        buffer.write(
            &widgets::sparkline(&series.cpu.elements(), width, None),
            area.x + 4,
            area.y,
            theme.style(Role::Cpu),
        );
        buffer.write("mem ", area.x, area.y + 1, theme.style(Role::Muted));
        buffer.write(
            &widgets::sparkline(&series.memory.elements(), width, None),
            area.x + 4,
            area.y + 1,
            theme.style(Role::Memory),
        );
        area = area.inset_edges(3, 0, 0, 0);
        if context
            .snapshot
            .metrics
            .latest
            .get(&container.id)
            .and_then(|s| s.gpu_percent)
            .is_some()
            && area.height >= 6
        {
            buffer.write("gpu ", area.x, area.y - 1, theme.style(Role::Muted));
            buffer.write(
                &widgets::sparkline(&series.gpu.elements(), width, Some(100.0)),
                area.x + 4,
                area.y - 1,
                theme.style(Role::Gpu),
            );
            area = area.inset_edges(1, 0, 0, 0);
        }
    }
    let lines = container_lines(
        container,
        detail.and_then(|d| d.details.as_ref()),
        context.snapshot.metrics.latest.get(&container.id),
    );
    render_key_values(buffer, area, &lines, context.state.detail_scroll, theme);
}

fn draw_processes(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    container: &Container,
    detail: Option<&ContainerDetailData>,
    context: &RenderContext<'_>,
) {
    let theme = context.theme;
    if !container.state.is_active() {
        buffer.write(
            "Container is not running",
            rect.x,
            rect.y,
            theme.style(Role::Muted),
        );
        return;
    }
    let Some(detail) = detail.filter(|d| !d.processes.is_empty()) else {
        buffer.write(
            "Loading processes…",
            rect.x,
            rect.y,
            theme.style(Role::Muted),
        );
        return;
    };
    buffer.write(
        &text::fit(
            "PID      CPU  COMMAND",
            rect.width as usize,
            Alignment::Left,
        ),
        rect.x,
        rect.y,
        theme.style(Role::TableHeader).bold(),
    );
    let body = rect.inset_edges(1, 0, 0, 0);
    let height = body.height.max(0) as usize;
    let start = context
        .state
        .detail_scroll
        .min(detail.processes.len().saturating_sub(height));
    for (offset, row) in detail.processes.iter().skip(start).take(height).enumerate() {
        let y = body.y + offset as i32;
        let mut x = body.x;
        x += buffer.write(
            &text::fit(&row.process.pid.to_string(), 8, Alignment::Left),
            x,
            y,
            theme.style(Role::Accent),
        );
        x += buffer.write(
            &text::fit(&row.process.cpu, 5, Alignment::Left),
            x,
            y,
            theme.style(Role::Muted),
        );
        x += buffer.write(&row.prefix(), x, y, theme.style(Role::Border));
        buffer.write_max(&row.process.command, x, y, theme.base(), body.max_x() - x);
    }
}

/// The stream indicator next to the pane tabs: live, paused (scrolled back) or ended.
pub fn log_status(
    detail: Option<&ContainerDetailData>,
    state: &AppState,
) -> Option<(&'static str, Role)> {
    match &detail?.log_state {
        LogStreamState::Idle => None,
        LogStreamState::Live if state.is_paused => Some(("⏸ updates paused ", Role::Warning)),
        LogStreamState::Live if state.log_follow => Some(("● live ", Role::Success)),
        LogStreamState::Live => Some(("⏸ paused · f follow ", Role::Warning)),
        LogStreamState::Ended(None) => Some(("■ ended ", Role::Muted)),
        LogStreamState::Ended(Some(_)) => Some(("✕ stream lost ", Role::Error)),
    }
}

fn draw_logs(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    detail: Option<&ContainerDetailData>,
    context: &RenderContext<'_>,
) {
    let theme = context.theme;
    let Some(detail) = detail else {
        buffer.write("Loading logs…", rect.x, rect.y, theme.style(Role::Muted));
        return;
    };
    if let LogStreamState::Ended(Some(error)) = &detail.log_state {
        // Keep what was received, and say why it stopped on the last line.
        let note = format!("log stream lost: {error} — reconnecting if the container is running");
        buffer.write(
            &text::truncate(&note, rect.width as usize),
            rect.x,
            rect.max_y() - 1,
            theme.style(Role::Error),
        );
        draw_log_lines(buffer, rect.inset_edges(0, 0, 1, 0), detail, context);
        return;
    }
    if detail.logs.is_empty() {
        let message = if detail.log_state == LogStreamState::Live {
            "Waiting for log output…"
        } else {
            "No log output"
        };
        buffer.write(message, rect.x, rect.y, theme.style(Role::Muted));
        return;
    }
    draw_log_lines(buffer, rect, detail, context);
}

fn draw_log_lines(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    detail: &ContainerDetailData,
    context: &RenderContext<'_>,
) {
    let theme = context.theme;
    if rect.height <= 0 {
        return;
    }
    let height = rect.height as usize;
    // Newest at the bottom; scrolling moves back in time.
    let scroll = context
        .state
        .detail_scroll
        .min(detail.logs.len().saturating_sub(height));
    let end = detail.logs.len() - scroll;
    let start = end.saturating_sub(height);
    for (offset, l) in detail.logs[start..end].iter().enumerate() {
        let style = if l.stream == LogStreamKind::Stderr {
            theme.style(Role::Warning)
        } else {
            theme.base()
        };
        buffer.write_max(&l.text, rect.x, rect.y + offset as i32, style, rect.width);
    }
}

fn project_lines(project: &ComposeProject) -> Vec<Line> {
    let total = project.containers().count();
    let mut lines = vec![line(
        "status",
        format!(
            "{} · {}/{total} running",
            project.aggregate_status().raw_value(),
            project.running_count()
        ),
        Some(compose_role(project.aggregate_status())),
    )];
    if let Some(directory) = &project.working_directory {
        lines.push(line("directory", directory.clone(), None));
    }
    for file in &project.config_files {
        lines.push(line("file", file.clone(), None));
    }
    for service in &project.services {
        lines.push(line(
            "service",
            format!(
                "{} {}/{}",
                service.name,
                service.running_count(),
                service.containers.len()
            ),
            Some(compose_role(service.aggregate_status())),
        ));
    }
    lines.push(line("", "", None));
    lines.push(line(
        "keys",
        "u start · U up · x stop · r restart · p pause · D down",
        Some(Role::Muted),
    ));
    lines
}

fn image_lines(image: &DockerImage, context: &RenderContext<'_>) -> Vec<Line> {
    let mut lines = vec![
        line("id", image.short_id(), None),
        line("size", format::bytes(image.size as f64), None),
    ];
    if image.created > 0 {
        lines.push(line(
            "created",
            format::age(image.created as f64, context.now),
            None,
        ));
    }
    for tag in &image.repo_tags {
        lines.push(line("tag", tag.clone(), None));
    }
    for digest in &image.repo_digests {
        lines.push(line("digest", digest.clone(), Some(Role::Muted)));
    }
    let users: Vec<&Container> = context
        .snapshot
        .containers
        .iter()
        .filter(|c| c.image_id == image.id)
        .collect();
    for c in &users {
        lines.push(line(
            "used by",
            c.name(),
            Some(state_role(&c.state, c.health())),
        ));
    }
    if users.is_empty() && image.is_dangling() {
        lines.push(line(
            "note",
            "dangling — removed by prune (X)",
            Some(Role::Warning),
        ));
    }
    lines
}

fn network_lines(network: &DockerNetwork, context: &RenderContext<'_>) -> Vec<Line> {
    let mut lines = vec![
        line("id", network.short_id(), None),
        line("driver", network.driver.clone(), None),
        line("scope", network.scope.clone(), None),
    ];
    if network.is_internal {
        lines.push(line("internal", "yes", Some(Role::Warning)));
    }
    if network.enable_ipv6 {
        lines.push(line("ipv6", "enabled", None));
    }
    for c in &network.ipam {
        lines.push(line(
            "subnet",
            if c.gateway.is_empty() {
                c.subnet.clone()
            } else {
                format!("{} via {}", c.subnet, c.gateway)
            },
            None,
        ));
    }
    for id in &network.container_ids {
        let name = context
            .snapshot
            .container(id)
            .map_or_else(|| id.chars().take(12).collect(), Container::name);
        lines.push(line("container", name, None));
    }
    if network.is_built_in() {
        lines.push(line(
            "note",
            "built-in network (cannot be removed)",
            Some(Role::Muted),
        ));
    }
    lines
}

fn volume_lines(volume: &DockerVolume) -> Vec<Line> {
    let mut lines = vec![
        line("driver", volume.driver.clone(), None),
        line("scope", volume.scope.clone(), None),
    ];
    if !volume.mountpoint.is_empty() {
        lines.push(line("mountpoint", volume.mountpoint.clone(), None));
    }
    if !volume.created_at.is_empty() {
        lines.push(line("created", volume.created_at.clone(), None));
    }
    if let Some(project) = volume.compose_project() {
        lines.push(line("compose", project, Some(Role::Accent)));
    }
    if let Some(size) = volume.size_bytes {
        lines.push(line("size", format::bytes(size as f64), None));
    }
    for (key, value) in &volume.labels {
        if !key.starts_with("com.docker.compose") {
            lines.push(line("label", format!("{key}={value}"), Some(Role::Muted)));
        }
    }
    lines
}

fn draw_lines(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    title: &str,
    lines: &[Line],
    context: &RenderContext<'_>,
) {
    let theme = context.theme;
    let inner = widgets::draw_box(
        buffer,
        rect,
        Some(title),
        None,
        theme.style(Role::Border),
        Some(theme.style(Role::Title).bold()),
        theme.border,
    );
    render_key_values(buffer, inner, lines, context.state.detail_scroll, theme);
}

fn render_key_values(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    lines: &[Line],
    scroll: usize,
    theme: &Theme,
) {
    if rect.is_empty() {
        return;
    }
    let key_width = (lines.iter().map(|l| text::width(&l.0)).max().unwrap_or(4) + 1).clamp(4, 11);
    let height = rect.height as usize;
    let start = scroll.min(lines.len().saturating_sub(height));
    for (offset, (key, value, role)) in lines.iter().skip(start).take(height).enumerate() {
        let y = rect.y + offset as i32;
        buffer.write(
            &text::fit(key, key_width, Alignment::Left),
            rect.x,
            y,
            theme.style(Role::Muted),
        );
        let style = role.map_or_else(|| theme.base(), |r| theme.style(r));
        buffer.write_max(
            value,
            rect.x + key_width as i32,
            y,
            style,
            rect.width - key_width as i32,
        );
    }
}

// MARK: - Overlays

const HELP_SECTIONS: [(&str, &[(&str, &str)]); 3] = [
    (
        "Navigate",
        &[
            ("↑↓ j k", "move selection"),
            ("PgUp PgDn", "page"),
            ("g G Home End", "first / last"),
            ("← → Tab h l", "switch view"),
            ("1-5", "jump to view"),
            ("mouse", "click, wheel"),
            ("Enter", "details / fold project"),
            ("v", "cycle detail pane"),
            ("d", "toggle details"),
            ("[ ]", "previous / next host"),
            ("H", "host list"),
        ],
    ),
    (
        "View",
        &[
            ("/", "filter (Esc clears)"),
            ("s / S", "sort key / direction"),
            ("a", "all · running · stopped"),
            ("T F2", "theme menu (live, save/revert)"),
            ("t", "cycle theme (not saved)"),
            ("f", "follow / pause the log stream"),
            ("space", "pause updates"),
            ("R F5", "refresh now"),
        ],
    ),
    (
        "Manage",
        &[
            ("u", "start / unpause"),
            ("x", "stop"),
            ("r", "restart"),
            ("p", "pause / unpause"),
            ("K", "kill"),
            ("D", "remove / compose down"),
            ("U", "compose up"),
            ("X", "prune this view"),
            ("q Ctrl-C", "quit"),
        ],
    ),
];

fn dialog_style(theme: &Theme) -> Style {
    Style::new(
        theme.color(Role::Foreground),
        theme.color(Role::DialogBackground),
    )
}

fn draw_host_picker(
    buffer: &mut ScreenBuffer,
    selected: usize,
    context: &RenderContext<'_>,
) -> Vec<(usize, Rect)> {
    let theme = context.theme;
    let hosts = context.hosts;
    if hosts.is_empty() {
        return Vec::new();
    }
    let width = (buffer.width - 4).min(96);
    let height = (buffer.height - 2).min(hosts.len() as i32 + 5);
    if width <= 20 || height <= 4 {
        return Vec::new();
    }
    let rect = buffer.bounds().centered(width, height);
    let dialog = dialog_style(theme);
    buffer.fill(rect, ' ', dialog);
    let title = format!("hosts ({})", hosts.len());
    let inner = widgets::draw_box(
        buffer,
        rect,
        Some(&title),
        Some("⏎ switch · esc close"),
        on(dialog, theme, Role::DialogBorder),
        Some(on(dialog, theme, Role::Title).bold()),
        theme.border,
    );
    // Size the name column to the names, so long endpoints keep the rest of the width.
    let name_width = (hosts
        .iter()
        .map(|h| text::width(&h.name))
        .max()
        .unwrap_or(0) as i32
        + 2)
    .clamp(6, 28);
    use Alignment::{Left, Right};
    use ColumnWidth::{Fixed, Flexible};
    let columns = vec![
        col("", Fixed(2), Left, 0),
        col("HOST", Fixed(name_width), Left, 0),
        col("STATUS", Fixed(12), Left, 0),
        col("UP", Fixed(7), Right, 0),
        col("CPU%", Fixed(6), Right, 2),
        col("MEM", Fixed(8), Right, 3),
        col("ENDPOINT", Flexible { min: 12, weight: 2 }, Left, 1),
    ];
    let rows: Vec<Vec<TableCell>> = hosts
        .iter()
        .enumerate()
        .map(|(index, host)| {
            let (status, role) = match host.connection {
                ConnectionState::Connected => ("connected", Role::StatusRunning),
                ConnectionState::Connecting => ("connecting", Role::Warning),
                ConnectionState::Disconnected { .. } => ("offline", Role::Error),
            };
            vec![
                TableCell::styled(
                    if index < 9 {
                        (index + 1).to_string()
                    } else {
                        String::new()
                    },
                    theme.style(Role::Muted),
                ),
                TableCell::styled(
                    format!(
                        "{}{}",
                        if index == context.state.active_host {
                            "▸ "
                        } else {
                            "  "
                        },
                        host.name
                    ),
                    theme.style(Role::Title),
                ),
                TableCell::styled(
                    format!("{} {status}", connection_symbol(&host.connection)),
                    theme.style(role),
                ),
                TableCell::new(if host.total > 0 || host.connection.is_connected() {
                    format!("{}/{}", host.running, host.total)
                } else {
                    "-".into()
                }),
                // Share of the whole host, like the cpu box ("x% of N cpu").
                TableCell::new(host.cpu_percent.map_or_else(
                    || "-".into(),
                    |c| format::percent(c / host.cpu_count.max(1) as f64, 1),
                )),
                TableCell::new(
                    host.memory_used
                        .map_or_else(|| "-".into(), |m| format::compact_bytes(m as f64)),
                ),
                TableCell::styled(host.description.clone(), theme.style(Role::Muted)),
            ]
        })
        .collect();
    let styles = TableStyles {
        header: on(dialog, theme, Role::TableHeader).bold(),
        row: dialog,
        selected: theme.selection().bold(),
        scroll_indicator: theme.style(Role::Muted),
    };
    let layout = widgets::draw_table(
        buffer,
        inner,
        &columns,
        &rows,
        Some(selected),
        0,
        styles,
        1,
        true,
    );
    if let ConnectionState::Disconnected { message, .. } =
        &hosts[selected.min(hosts.len() - 1)].connection
    {
        if inner.height > hosts.len() as i32 + 2 {
            buffer.write_max(
                message,
                inner.x + 1,
                inner.max_y() - 1,
                on(dialog, theme, Role::Error),
                inner.width - 2,
            );
        }
    }
    layout.row_rects
}

fn draw_help(buffer: &mut ScreenBuffer, context: &RenderContext<'_>) {
    let theme = context.theme;
    let row_count: usize = HELP_SECTIONS.iter().map(|(_, b)| b.len() + 2).sum();
    let height = (buffer.height - 2).min(row_count as i32 + 3);
    let width = (buffer.width - 4).min(56);
    let rect = buffer.bounds().centered(width, height);
    let dialog = dialog_style(theme);
    buffer.fill(rect, ' ', dialog);
    let title = format!("dtop {APP_VERSION} — keys");
    let inner = widgets::draw_box(
        buffer,
        rect,
        Some(&title),
        Some("any key closes"),
        on(dialog, theme, Role::DialogBorder),
        Some(on(dialog, theme, Role::Title).bold()),
        theme.border,
    );
    let mut y = inner.y;
    for (title, bindings) in HELP_SECTIONS {
        if y >= inner.max_y() {
            break;
        }
        buffer.write(
            title,
            inner.x + 1,
            y,
            on(dialog, theme, Role::Accent).bold(),
        );
        y += 1;
        for (keys, action) in bindings {
            if y >= inner.max_y() {
                break;
            }
            buffer.write(
                &text::fit(keys, 15, Alignment::Left),
                inner.x + 2,
                y,
                on(dialog, theme, Role::Title),
            );
            buffer.write_max(
                action,
                inner.x + 18,
                y,
                on(dialog, theme, Role::Muted),
                inner.width - 18,
            );
            y += 1;
        }
        y += 1;
    }
}

fn draw_confirmation(
    buffer: &mut ScreenBuffer,
    confirmation: &Confirmation,
    context: &RenderContext<'_>,
) -> (Option<Rect>, Option<Rect>) {
    let theme = context.theme;
    let width = (buffer.width - 4).min(64);
    let message_lines = text::wrap(
        &confirmation.operation.confirmation_message(),
        (width - 4).max(10) as usize,
    );
    let height = (buffer.height - 2).min(message_lines.len() as i32 + 6);
    let rect = buffer.bounds().centered(width, height);
    let dialog = dialog_style(theme);
    buffer.fill(rect, ' ', dialog);
    let title = confirmation.operation.confirmation_title();
    let inner = widgets::draw_box(
        buffer,
        rect,
        Some(&title),
        None,
        on(dialog, theme, Role::Warning),
        Some(on(dialog, theme, Role::Warning).bold()),
        theme.border,
    );
    if inner.height < 3 {
        return (None, None);
    }
    for (offset, l) in message_lines.iter().enumerate() {
        let y = inner.y + 1 + offset as i32;
        if y < inner.max_y() - 2 {
            buffer.write(l, inner.x + 1, y, dialog);
        }
    }
    let (yes, no) = ("  Yes (y)  ", "  No (n)  ");
    let buttons_width = (text::width(yes) + text::width(no) + 3) as i32;
    let y = inner.max_y() - 1;
    let start = inner.x + ((inner.width - buttons_width) / 2).max(0);
    let focused = theme.selection().bold();
    let unfocused = on(dialog, theme, Role::Muted);
    let (yes_style, no_style) = if confirmation.confirm_focused {
        (focused.with_bg(theme.color(Role::Error)), unfocused)
    } else {
        (unfocused, focused)
    };
    buffer.write(yes, start, y, yes_style);
    let no_x = start + text::width(yes) as i32 + 3;
    buffer.write(no, no_x, y, no_style);
    (
        Some(Rect::new(start, y, text::width(yes) as i32, 1)),
        Some(Rect::new(no_x, y, text::width(no) as i32, 1)),
    )
}

// MARK: - Theme menu

pub const THEME_PANEL_WIDTH: i32 = 58;

/// The theme menu docked on the right while the rest of the UI keeps rendering with the draft.
fn draw_theme_editor(buffer: &mut ScreenBuffer, editor: &ThemeEditor) -> Vec<(usize, Rect)> {
    let theme = &editor.draft;
    let bounds = buffer.bounds();
    let width = THEME_PANEL_WIDTH.min(bounds.width - 2);
    let rect = Rect::new(bounds.max_x() - width, 1, width, (bounds.height - 2).max(0));
    if rect.width < 30 || rect.height < 12 {
        return Vec::new();
    }
    let panel = dialog_style(theme);
    buffer.fill(rect, ' ', panel);
    let title = format!("theme · {}", editor.draft.name);
    let inner = widgets::draw_box(
        buffer,
        rect,
        Some(&title),
        Some(if editor.is_dirty() {
            "unsaved"
        } else {
            "saved"
        }),
        on(panel, theme, Role::DialogBorder),
        Some(on(panel, theme, Role::Title).bold()),
        theme.border,
    );
    let mut y = inner.y;
    let mut x = inner.x + 1;
    for (label, role) in [
        ("● running", Role::StatusRunning),
        ("◐ paused", Role::StatusPaused),
        ("◌ exited", Role::StatusExited),
        ("↻ restart", Role::StatusRestarting),
        ("✕ dead", Role::StatusDead),
    ] {
        x += buffer.write_max(
            &format!("{label}  "),
            x,
            y,
            on(panel, theme, role),
            inner.max_x() - x,
        );
    }
    y += 1;
    let bar_width = (inner.width - 2).max(4);
    for column in 0..bar_width {
        let fraction = f64::from(column) / f64::from((bar_width - 1).max(1));
        buffer.put(
            '█',
            inner.x + 1 + column,
            y,
            panel.with_fg(theme.gradient(fraction)),
        );
    }
    y += 1;
    x = inner.x + 1;
    x += buffer.write(" selected row ", x, y, theme.selection().bold());
    x += buffer.write("  ", x, y, panel);
    for (label, role) in [
        ("accent", Role::Accent),
        ("muted", Role::Muted),
        ("warn", Role::Warning),
        ("error", Role::Error),
    ] {
        x += buffer.write_max(
            &format!("{label} "),
            x,
            y,
            on(panel, theme, role),
            inner.max_x() - x,
        );
    }
    y += 1;
    buffer.write(
        &"─".repeat(inner.width.max(0) as usize),
        inner.x,
        y,
        on(panel, theme, Role::Border),
    );
    y += 1;
    // Rows, scrolled to keep the selection visible; the bottom four lines are reserved.
    let list_height = (inner.max_y() - y - 4).max(1) as usize;
    let rows = theme_editor::rows();
    let offset = widgets::scroll_offset(
        Some(editor.selected),
        editor.selected.saturating_sub(list_height / 2),
        list_height,
        rows.len(),
    );
    let mut hits = Vec::new();
    for (l, index) in (offset..rows.len().min(offset + list_height)).enumerate() {
        let row = rows[index];
        let row_y = y + l as i32;
        let is_selected = index == editor.selected;
        let row_rect = Rect::new(inner.x, row_y, inner.width, 1);
        let base = if is_selected {
            theme.selection()
        } else {
            panel
        };
        buffer.fill(row_rect, ' ', base);
        hits.push((index, row_rect));
        let mut column = inner.x + 1;
        column += buffer.write(
            if editor.is_modified(row) { "•" } else { " " },
            column,
            row_y,
            on(base, theme, Role::Warning),
        );
        column += buffer.write(
            &format!("{} ", text::fit(&row.label(), 19, Alignment::Left)),
            column,
            row_y,
            if is_selected { base.bold() } else { base },
        );
        if let Row::Role(role) = row {
            let swatch = if editor.draft.color(role) == Color::Default {
                theme.color(Role::Foreground)
            } else {
                editor.draft.color(role)
            };
            column += buffer.write("████", column, row_y, base.with_fg(swatch));
            column += buffer.write(" ", column, row_y, base);
            let value_style = on(
                base,
                theme,
                if is_selected {
                    Role::SelectionForeground
                } else {
                    Role::Muted
                },
            );
            column += buffer.write(
                &format!(
                    "{} ",
                    text::fit(&editor.value_text(row), 12, Alignment::Left)
                ),
                column,
                row_y,
                value_style,
            );
            if let Some((r, g, b)) = editor
                .draft
                .color(role)
                .rgb_components()
                .filter(|_| is_selected)
            {
                for (channel, value) in [
                    (theme_editor::Channel::Red, r),
                    (theme_editor::Channel::Green, g),
                    (theme_editor::Channel::Blue, b),
                ] {
                    if column >= inner.max_x() - 4 {
                        break;
                    }
                    let text = format!(
                        "{}{} ",
                        channel.label(),
                        text::fit(&value.to_string(), 3, Alignment::Right)
                    );
                    let style = if channel == editor.channel {
                        base.adding(Attributes::REVERSE | Attributes::BOLD)
                    } else {
                        base
                    };
                    column += buffer.write_max(&text, column, row_y, style, inner.max_x() - column);
                }
            }
        } else {
            let style = on(
                base,
                theme,
                if is_selected {
                    Role::SelectionForeground
                } else {
                    Role::Accent
                },
            );
            buffer.write_max(
                &editor.value_text(row),
                column,
                row_y,
                style,
                inner.max_x() - column,
            );
        }
    }
    let status_y = inner.max_y() - 3;
    buffer.write(
        &"─".repeat(inner.width.max(0) as usize),
        inner.x,
        status_y,
        on(panel, theme, Role::Border),
    );
    let field_y = status_y + 1;
    let muted = on(panel, theme, Role::Muted);
    match &editor.field {
        Field::None => {
            if let Some(message) = &editor.message {
                buffer.write_max(
                    &format!(" {message}"),
                    inner.x,
                    field_y,
                    on(panel, theme, Role::Warning),
                    inner.width,
                );
            } else if matches!(editor.selected_row(), Row::Role(_)) {
                let hint = format!(
                    " ←→ change {} · tab channel · ⏎ type a color",
                    editor.channel.label()
                );
                buffer.write_max(&hint, inner.x, field_y, muted, inner.width);
            } else {
                buffer.write_max(" ←→ or ⏎ to change", inner.x, field_y, muted, inner.width);
            }
        }
        Field::ColorInput(text) | Field::SaveName(text) => {
            let prompt = if matches!(editor.field, Field::ColorInput(_)) {
                " color: "
            } else {
                " save as: "
            };
            let mut column = inner.x;
            column += buffer.write(
                prompt,
                column,
                field_y,
                on(panel, theme, Role::Accent).bold(),
            );
            column += buffer.write_max(text, column, field_y, panel, inner.max_x() - column - 1);
            buffer.write("▏", column, field_y, on(panel, theme, Role::Accent));
            if let Some(message) = &editor.message {
                buffer.write_max(
                    message,
                    inner.x + 1,
                    field_y + 1,
                    on(panel, theme, Role::Error),
                    inner.width - 1,
                );
                return hits;
            }
            if matches!(editor.field, Field::SaveName(_)) {
                buffer.write(" ⏎ save · esc back", inner.x, field_y + 1, muted);
                return hits;
            }
        }
        Field::ConfirmDiscard => {
            buffer.write_max(
                " Discard unsaved theme changes? y discard · n keep editing · s save",
                inner.x,
                field_y,
                on(panel, theme, Role::Warning).bold(),
                inner.width,
            );
            return hits;
        }
    }
    buffer.write_max(
        " ↑↓ select · d reset · r revert all · s save · esc close",
        inner.x,
        field_y + 1,
        muted,
        inner.width,
    );
    hits
}
