//! All UI state as plain values: the reducer mutates `AppState`, views read it with a
//! `DockerSnapshot` of the active host.

use std::collections::{BTreeMap, BTreeSet};

use dtop_core::logstream::LogLine;
use dtop_core::models::{
    Container, ContainerDetails, ContainerState, DockerImage, DockerNetwork, DockerVersion,
    DockerVolume, SystemInfo,
};
use dtop_core::monitoring::{MetricsStore, ProcessTreeRow};
use dtop_core::ContainerAction;
use dtop_ui::Theme;

use crate::config::AppConfiguration;
use crate::operation::DockerOperation;
use crate::theme_editor::ThemeEditor;

pub const APP_NAME: &str = "dtop";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_TAGLINE: &str = "Docker-first terminal observability and control";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tab {
    Containers,
    Compose,
    Images,
    Networks,
    Volumes,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Containers,
        Tab::Compose,
        Tab::Images,
        Tab::Networks,
        Tab::Volumes,
    ];

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn title(self) -> &'static str {
        match self {
            Tab::Containers => "Containers",
            Tab::Compose => "Compose",
            Tab::Images => "Images",
            Tab::Networks => "Networks",
            Tab::Volumes => "Volumes",
        }
    }

    pub fn shortcut(self) -> char {
        char::from(b'1' + self.index() as u8)
    }

    pub fn next(self) -> Tab {
        Tab::ALL[(self.index() + 1) % Tab::ALL.len()]
    }

    pub fn previous(self) -> Tab {
        Tab::ALL[(self.index() + Tab::ALL.len() - 1) % Tab::ALL.len()]
    }

    /// Tabs whose lists come from the slower "inventory" refresh.
    pub fn uses_inventory(self) -> bool {
        matches!(self, Tab::Images | Tab::Networks | Tab::Volumes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortKey {
    Name,
    Cpu,
    Memory,
    Gpu,
    Status,
    Created,
    Image,
}

impl SortKey {
    pub const ALL: [SortKey; 7] = [
        SortKey::Name,
        SortKey::Cpu,
        SortKey::Memory,
        SortKey::Gpu,
        SortKey::Status,
        SortKey::Created,
        SortKey::Image,
    ];

    pub fn raw_value(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Cpu => "cpu",
            SortKey::Memory => "memory",
            SortKey::Gpu => "gpu",
            SortKey::Status => "status",
            SortKey::Created => "created",
            SortKey::Image => "image",
        }
    }

    pub fn parse(raw: &str) -> Option<SortKey> {
        SortKey::ALL.into_iter().find(|k| k.raw_value() == raw)
    }

    pub fn next(self) -> SortKey {
        let i = SortKey::ALL.iter().position(|k| *k == self).unwrap_or(0);
        SortKey::ALL[(i + 1) % SortKey::ALL.len()]
    }

    /// Whether descending is the natural first direction (biggest consumers first).
    pub fn prefers_descending(self) -> bool {
        matches!(
            self,
            SortKey::Cpu | SortKey::Memory | SortKey::Gpu | SortKey::Created
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusFilter {
    All,
    Running,
    Stopped,
}

impl StatusFilter {
    pub fn raw_value(self) -> &'static str {
        match self {
            StatusFilter::All => "all",
            StatusFilter::Running => "running",
            StatusFilter::Stopped => "stopped",
        }
    }

    pub fn next(self) -> StatusFilter {
        match self {
            StatusFilter::All => StatusFilter::Running,
            StatusFilter::Running => StatusFilter::Stopped,
            StatusFilter::Stopped => StatusFilter::All,
        }
    }

    pub fn matches(self, state: &ContainerState) -> bool {
        match self {
            StatusFilter::All => true,
            StatusFilter::Running => state.is_active(),
            StatusFilter::Stopped => !state.is_active(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetailPane {
    Overview,
    Processes,
    Logs,
}

impl DetailPane {
    pub const ALL: [DetailPane; 3] = [
        DetailPane::Overview,
        DetailPane::Processes,
        DetailPane::Logs,
    ];

    pub fn title(self) -> &'static str {
        match self {
            DetailPane::Overview => "Overview",
            DetailPane::Processes => "Processes",
            DetailPane::Logs => "Logs",
        }
    }

    pub fn next(self) -> DetailPane {
        match self {
            DetailPane::Overview => DetailPane::Processes,
            DetailPane::Processes => DetailPane::Logs,
            DetailPane::Logs => DetailPane::Overview,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub text: String,
    pub level: NoticeLevel,
    pub expires_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Confirmation {
    pub operation: DockerOperation,
    /// Which button has focus; destructive dialogs default to "No".
    pub confirm_focused: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputMode {
    Normal,
    Filter,
    Help,
    Confirm(Confirmation),
    /// The host list (`H`) with the highlighted row.
    HostPicker(usize),
    /// Boxed: the editor carries several full themes.
    ThemeEditor(Box<ThemeEditor>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppState {
    pub tab: Tab,
    /// Selected item ID per tab, so refreshes that reorder rows keep the selection.
    pub selected_ids: BTreeMap<Tab, String>,
    /// Last selected index per tab, for when the selected item disappears.
    pub selected_indexes: BTreeMap<Tab, usize>,
    pub scroll_offsets: BTreeMap<Tab, usize>,
    pub sort_key: SortKey,
    pub sort_descending: bool,
    pub filter_text: String,
    pub status_filter: StatusFilter,
    pub show_details: bool,
    pub detail_pane: DetailPane,
    pub detail_scroll: usize,
    /// The logs pane sticks to the newest line until the user scrolls back (or presses `f`).
    pub log_follow: bool,
    pub collapsed_projects: BTreeSet<String>,
    pub mode: InputMode,
    pub notice: Option<Notice>,
    pub theme_name: String,
    pub is_paused: bool,
    pub should_quit: bool,
    /// Names of the monitored hosts, in order, and which one the views show.
    pub host_names: Vec<String>,
    pub active_host: usize,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            tab: Tab::Containers,
            selected_ids: BTreeMap::new(),
            selected_indexes: BTreeMap::new(),
            scroll_offsets: BTreeMap::new(),
            sort_key: SortKey::Cpu,
            sort_descending: true,
            filter_text: String::new(),
            status_filter: StatusFilter::All,
            show_details: true,
            detail_pane: DetailPane::Overview,
            detail_scroll: 0,
            log_follow: true,
            collapsed_projects: BTreeSet::new(),
            mode: InputMode::Normal,
            notice: None,
            theme_name: "harbor".into(),
            is_paused: false,
            should_quit: false,
            host_names: Vec::new(),
            active_host: 0,
        }
    }
}

impl AppState {
    pub fn from_configuration(configuration: &AppConfiguration) -> AppState {
        AppState {
            sort_key: configuration.sort_key,
            sort_descending: configuration
                .sort_descending
                .unwrap_or_else(|| configuration.sort_key.prefers_descending()),
            status_filter: if configuration.show_stopped {
                StatusFilter::All
            } else {
                StatusFilter::Running
            },
            theme_name: configuration.theme_name.clone(),
            ..AppState::default()
        }
    }

    pub fn has_several_hosts(&self) -> bool {
        self.host_names.len() > 1
    }

    /// Makes `index` the active host. Selections and scroll positions belong to the previous
    /// host's lists, so they reset; filter, sort and tab carry over.
    pub fn switch_host(&mut self, index: usize) {
        if index >= self.host_names.len() || index == self.active_host {
            return;
        }
        self.active_host = index;
        self.selected_ids.clear();
        self.selected_indexes.clear();
        self.scroll_offsets.clear();
        self.detail_scroll = 0;
        self.log_follow = true;
    }

    pub fn post(
        &mut self,
        text: impl Into<String>,
        level: NoticeLevel,
        now: f64,
        duration: Option<f64>,
    ) {
        self.notice = Some(Notice {
            text: text.into(),
            level,
            expires_at: duration.map(|d| now + d),
        });
    }

    pub fn theme_editor(&self) -> Option<&ThemeEditor> {
        match &self.mode {
            InputMode::ThemeEditor(editor) => Some(editor),
            _ => None,
        }
    }

    pub fn open_theme_editor(&mut self, current: Theme, available: Vec<Theme>) {
        self.mode = InputMode::ThemeEditor(Box::new(ThemeEditor::new(current, available)));
    }

    /// After trying to persist a theme: success closes the editor and makes the theme current;
    /// failure keeps the editor (and the draft) open with the reason.
    pub fn finish_theme_save(
        &mut self,
        name: &str,
        error: Option<String>,
        location: Option<String>,
        now: f64,
    ) {
        let InputMode::ThemeEditor(editor) = &mut self.mode else {
            return;
        };
        if let Some(error) = error {
            editor.field = crate::theme_editor::Field::None;
            editor.message = Some(error);
            return;
        }
        self.theme_name = name.to_string();
        self.mode = InputMode::Normal;
        let suffix = location.map(|l| format!(" to {l}")).unwrap_or_default();
        self.post(
            format!("Saved theme '{name}'{suffix}"),
            NoticeLevel::Success,
            now,
            Some(6.0),
        );
    }

    pub fn expire_notice(&mut self, now: f64) {
        if self
            .notice
            .as_ref()
            .and_then(|n| n.expires_at)
            .is_some_and(|e| now >= e)
        {
            self.notice = None;
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Disconnected {
        message: String,
        retry_at: Option<f64>,
    },
}

impl ConnectionState {
    pub fn is_connected(&self) -> bool {
        *self == ConnectionState::Connected
    }
}

/// Where the followed log stream of the selected container stands.
#[derive(Debug, Clone, PartialEq)]
pub enum LogStreamState {
    /// Not streaming (fetched once, or nothing asked for).
    Idle,
    /// `logs -f` is open and new lines are appended as they arrive.
    Live,
    /// The stream ended: the container stopped, or the connection broke (with the reason).
    Ended(Option<String>),
}

/// Detail panel data for one container, loaded lazily for the selection only.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerDetailData {
    pub container_id: String,
    pub details: Option<ContainerDetails>,
    pub processes: Vec<ProcessTreeRow>,
    pub logs: Vec<LogLine>,
    pub log_state: LogStreamState,
    pub error: Option<String>,
    pub loaded_at: f64,
}

impl ContainerDetailData {
    /// Lines kept for a followed stream; older lines scroll out.
    pub const MAXIMUM_LOG_LINES: usize = 5_000;

    pub fn new(container_id: &str) -> Self {
        ContainerDetailData {
            container_id: container_id.into(),
            details: None,
            processes: Vec::new(),
            logs: Vec::new(),
            log_state: LogStreamState::Idle,
            error: None,
            loaded_at: 0.0,
        }
    }

    /// Appends streamed lines, keeping at most `MAXIMUM_LOG_LINES`; returns how many lines were
    /// dropped from the front.
    pub fn append_logs(&mut self, lines: Vec<LogLine>) -> usize {
        self.logs.extend(lines);
        let overflow = self.logs.len().saturating_sub(Self::MAXIMUM_LOG_LINES);
        if overflow > 0 {
            self.logs.drain(..overflow);
        }
        overflow
    }
}

/// Everything the UI knows about one Docker host at one moment.
#[derive(Debug, Clone)]
pub struct DockerSnapshot {
    pub endpoint: String,
    pub connection: ConnectionState,
    pub version: Option<DockerVersion>,
    pub info: Option<SystemInfo>,
    pub containers: Vec<Container>,
    pub images: Vec<DockerImage>,
    pub networks: Vec<DockerNetwork>,
    pub volumes: Vec<DockerVolume>,
    pub metrics: MetricsStore,
    pub detail: Option<ContainerDetailData>,
    pub last_updated: Option<f64>,
    /// Errors from secondary lists (e.g. volumes) that do not mark the connection down.
    pub warnings: Vec<String>,
}

impl Default for DockerSnapshot {
    fn default() -> Self {
        DockerSnapshot {
            endpoint: String::new(),
            connection: ConnectionState::Connecting,
            version: None,
            info: None,
            containers: Vec::new(),
            images: Vec::new(),
            networks: Vec::new(),
            volumes: Vec::new(),
            metrics: MetricsStore::default(),
            detail: None,
            last_updated: None,
            warnings: Vec::new(),
        }
    }
}

impl DockerSnapshot {
    pub fn container(&self, id: &str) -> Option<&Container> {
        self.containers.iter().find(|c| c.id == id)
    }

    pub fn running_count(&self) -> usize {
        self.containers
            .iter()
            .filter(|c| c.state == ContainerState::Running)
            .count()
    }

    pub fn stopped_count(&self) -> usize {
        self.containers
            .iter()
            .filter(|c| !c.state.is_active())
            .count()
    }
}

/// One line about a host for the header chips and the host list.
#[derive(Debug, Clone, PartialEq)]
pub struct HostSummary {
    pub name: String,
    pub description: String,
    pub connection: ConnectionState,
    pub running: usize,
    pub total: usize,
    pub cpu_percent: Option<f64>,
    pub cpu_count: i64,
    pub memory_used: Option<u64>,
    pub memory_total: u64,
}

impl HostSummary {
    pub fn new(name: &str, description: &str, snapshot: &DockerSnapshot) -> Self {
        let connected = snapshot.connection.is_connected() && snapshot.last_updated.is_some();
        HostSummary {
            name: name.into(),
            description: description.into(),
            connection: snapshot.connection.clone(),
            running: snapshot.running_count(),
            total: snapshot.containers.len(),
            cpu_percent: connected.then_some(snapshot.metrics.total_latest.cpu_percent),
            cpu_count: snapshot.info.as_ref().map_or(0, |i| i.cpu_count),
            memory_used: connected.then_some(snapshot.metrics.total_latest.memory_usage),
            memory_total: snapshot
                .info
                .as_ref()
                .map_or(0, |i| i.memory_total.max(0) as u64),
        }
    }
}

/// `start` → `Start`.
pub fn capitalized_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Present-progressive and past forms of container actions.
pub fn progress_verb(action: &ContainerAction) -> &'static str {
    match action {
        ContainerAction::Start => "Starting",
        ContainerAction::Stop { .. } => "Stopping",
        ContainerAction::Restart { .. } => "Restarting",
        ContainerAction::Kill { .. } => "Killing",
        ContainerAction::Pause => "Pausing",
        ContainerAction::Unpause => "Resuming",
    }
}

pub fn past_verb(action: &ContainerAction) -> &'static str {
    match action {
        ContainerAction::Start => "Started",
        ContainerAction::Stop { .. } => "Stopped",
        ContainerAction::Restart { .. } => "Restarted",
        ContainerAction::Kill { .. } => "Killed",
        ContainerAction::Pause => "Paused",
        ContainerAction::Unpause => "Resumed",
    }
}
