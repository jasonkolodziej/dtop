//! The main loop. It owns the terminal and all UI state. Producers send `LoopEvent`s over one
//! channel: an input thread, two timers, one Docker event stream per host, and worker threads
//! for refreshes, detail loads, operations and the followed log stream.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use dtop_core::host::DockerHost;
use dtop_core::logstream::LogLine;
use dtop_core::models::DockerEvent;
use dtop_core::monitoring::{jitter_random, Backoff};
use dtop_core::{format, DockerApi, StreamCancellation, StreamItem};
use dtop_ui::render::{seq, FrameRenderer};
use dtop_ui::{ColorMode, InputEvent, InputParser, Terminal, TerminalError, TerminalSize, Theme};

use crate::config::AppConfiguration;
use crate::list;
use crate::log_policy::{self, Decision, Target};
use crate::operation::OperationResult;
use crate::reducer::{AppCommand, HitMap};
use crate::refresher::{describe, DataRefresher};
use crate::state::*;
use crate::theme_files::{LocalFileSystem, ThemeLoader, ThemeStore};
use crate::views::{self, RenderContext, ScreenLayout};

/// Everything that can wake the main loop.
pub enum LoopEvent {
    Input(Vec<u8>),
    InputClosed,
    FlushInput(u64),
    Tick,
    FastTick,
    DockerEvent(usize, DockerEvent),
    Refreshed(usize, Box<DockerSnapshot>),
    DetailLoaded {
        generation: u64,
        host: usize,
        pane: DetailPane,
        data: Box<ContainerDetailData>,
    },
    OperationFinished(usize, OperationResult),
    LogLines {
        generation: u64,
        host: usize,
        container_id: String,
        lines: Vec<LogLine>,
    },
    LogStreamEnded {
        generation: u64,
        host: usize,
        container_id: String,
        error: Option<String>,
    },
    Terminate,
}

/// One Docker host as the runtime sees it.
#[derive(Clone)]
pub struct HostConnection {
    pub host: DockerHost,
    pub api: Arc<dyn DockerApi>,
    pub refresher: Arc<DataRefresher>,
}

struct Session {
    connection: HostConnection,
    snapshot: DockerSnapshot,
    is_refreshing: bool,
    pending_refresh: Option<bool>,
    event_refresh_due: bool,
}

struct LogStream {
    generation: u64,
    host: usize,
    container_id: String,
    cancelled: Arc<AtomicBool>,
    cancellation: Arc<StreamCancellation>,
}

struct LogRestart {
    host: usize,
    container_id: String,
    at: f64,
}

impl LogStream {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.cancellation.cancel();
    }
}

/// Set from signal handlers; the fast tick turns it into `Terminate`.
static TERMINATE_REQUESTED: AtomicBool = AtomicBool::new(false);

pub struct AppController {
    configuration: AppConfiguration,
    terminal: Terminal,
    sessions: Vec<Session>,
    theme_loader: ThemeLoader,
    theme_names: Vec<String>,
    theme_store: ThemeStore,
    state: AppState,
    theme: Theme,
    renderer: FrameRenderer,
    hit_map: HitMap,
    parser: InputParser,
    input_generation: u64,
    size: TerminalSize,
    environment: HashMap<String, String>,
    tick_count: u64,
    detail_key: Option<String>,
    detail_generation: u64,
    is_loading_detail: bool,
    last_detail_load: f64,
    initial_notice: Option<String>,
    log_stream: Option<LogStream>,
    log_generation: u64,
    log_restart: Option<LogRestart>,
    log_tail: usize,
    /// Lines that arrived while updates were paused; added when updates resume.
    paused_log_lines: Vec<LogLine>,
    sender: Sender<LoopEvent>,
    receiver: Receiver<LoopEvent>,
    stop: Arc<AtomicBool>,
}

pub struct ControllerOptions {
    pub configuration: AppConfiguration,
    pub hosts: Vec<HostConnection>,
    pub notice: Option<String>,
    pub theme: Theme,
    pub color_mode: ColorMode,
    pub theme_loader: ThemeLoader,
    pub theme_names: Vec<String>,
    pub theme_store: ThemeStore,
    pub environment: HashMap<String, String>,
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(body)
        .map(|_| ())
        .map_err(|error| {
            std::io::Error::new(error.kind(), format!("could not start {name}: {error}"))
        })
}

fn read_text_file(path: &str) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

impl AppController {
    pub fn new(options: ControllerOptions) -> Self {
        let (sender, receiver) = mpsc::channel();
        let terminal = Terminal::new();
        let size = terminal.size(&options.environment);
        let history = options.configuration.history_size;
        let sessions: Vec<Session> = options
            .hosts
            .into_iter()
            .map(|connection| {
                let mut snapshot = connection.refresher.snapshot();
                snapshot.endpoint = connection.host.description();
                snapshot.metrics = dtop_core::monitoring::MetricsStore::new(history);
                Session {
                    connection,
                    snapshot,
                    is_refreshing: false,
                    pending_refresh: None,
                    event_refresh_due: false,
                }
            })
            .collect();
        let mut state = AppState::from_configuration(&options.configuration);
        state.theme_name = options.theme.name.clone();
        state.host_names = sessions
            .iter()
            .map(|s| s.connection.host.name.clone())
            .collect();
        AppController {
            log_tail: 300,
            configuration: options.configuration,
            terminal,
            sessions,
            theme_loader: options.theme_loader,
            theme_names: options.theme_names,
            theme_store: options.theme_store,
            state,
            theme: options.theme,
            renderer: FrameRenderer::new(options.color_mode),
            hit_map: HitMap::new(),
            parser: InputParser::new(),
            input_generation: 0,
            size,
            environment: options.environment,
            tick_count: 0,
            detail_key: None,
            detail_generation: 0,
            is_loading_detail: false,
            last_detail_load: 0.0,
            initial_notice: options.notice,
            log_stream: None,
            log_generation: 0,
            log_restart: None,
            paused_log_lines: Vec::new(),
            sender,
            receiver,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn now(&self) -> f64 {
        format::now()
    }

    fn active(&self) -> usize {
        self.state.active_host
    }

    fn snapshot(&self) -> &DockerSnapshot {
        &self.sessions[self.active()].snapshot
    }

    fn snapshot_mut(&mut self) -> &mut DockerSnapshot {
        let active = self.active();
        &mut self.sessions[active].snapshot
    }

    pub fn run(mut self) -> Result<(), TerminalError> {
        self.terminal.enable_raw_mode()?;
        install_signal_handlers();
        install_panic_hook();
        self.terminal.enter_fullscreen(self.configuration.mouse);
        self.terminal.write(&seq::set_title("dtop"));
        let startup = (|| -> std::io::Result<()> {
            self.start_input_thread()?;
            self.start_timers()?;
            for index in 0..self.sessions.len() {
                self.start_docker_events(index)?;
            }
            Ok(())
        })();
        if let Err(error) = startup {
            self.shutdown();
            return Err(TerminalError::RuntimeFailed(format!(
                "{error} — check system thread limits and try again"
            )));
        }
        for index in 0..self.sessions.len() {
            self.request_refresh(index, true);
        }
        if let Some(notice) = self.initial_notice.take() {
            let now = self.now();
            self.state
                .post(notice, NoticeLevel::Warning, now, Some(10.0));
        }
        self.render();
        while let Ok(event) = self.receiver.recv() {
            self.handle(event);
            // Drain whatever else is queued before drawing, so bursts render once.
            while let Ok(event) = self.receiver.try_recv() {
                if self.state.should_quit {
                    break;
                }
                self.handle(event);
            }
            if self.state.should_quit {
                break;
            }
            self.render();
        }
        self.shutdown();
        Ok(())
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(stream) = self.log_stream.take() {
            stream.cancel();
        }
        self.terminal.leave_fullscreen();
        self.terminal.restore();
    }

    // MARK: - Producers

    fn start_input_thread(&self) -> std::io::Result<()> {
        let sender = self.sender.clone();
        spawn("dtop-input", move || {
            let terminal = Terminal::new();
            loop {
                let bytes = terminal.read(4096);
                if bytes.is_empty() {
                    let _ = sender.send(LoopEvent::InputClosed);
                    return;
                }
                if sender.send(LoopEvent::Input(bytes)).is_err() {
                    return;
                }
            }
        })
    }

    fn start_timers(&self) -> std::io::Result<()> {
        let interval = Duration::from_secs_f64(self.configuration.refresh_interval);
        for (name, period, fast) in [
            ("dtop-tick", interval, false),
            ("dtop-fast-tick", Duration::from_millis(250), true),
        ] {
            let sender = self.sender.clone();
            let stop = Arc::clone(&self.stop);
            spawn(name, move || {
                while !stop.load(Ordering::SeqCst) {
                    std::thread::sleep(period);
                    if fast && TERMINATE_REQUESTED.load(Ordering::SeqCst) {
                        let _ = sender.send(LoopEvent::Terminate);
                    }
                    let event = if fast {
                        LoopEvent::FastTick
                    } else {
                        LoopEvent::Tick
                    };
                    if sender.send(event).is_err() {
                        return;
                    }
                }
            })?;
        }
        Ok(())
    }

    fn start_docker_events(&self, host: usize) -> std::io::Result<()> {
        let api = Arc::clone(&self.sessions[host].connection.api);
        let sender = self.sender.clone();
        let stop = Arc::clone(&self.stop);
        spawn("dtop-events", move || {
            let mut backoff = Backoff::new(1.0, 30.0, 2.0, 0.2);
            while !stop.load(Ordering::SeqCst) {
                let subscription = api.events();
                // Wake periodically to notice shutdown even when the daemon is quiet.
                loop {
                    match subscription
                        .receiver
                        .recv_timeout(Duration::from_millis(500))
                    {
                        Ok(StreamItem::Item(event)) => {
                            backoff.reset();
                            if sender.send(LoopEvent::DockerEvent(host, event)).is_err() {
                                return;
                            }
                        }
                        // Connection problems surface through polling; just retry here.
                        Ok(StreamItem::End | StreamItem::Failed(_))
                        | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if stop.load(Ordering::SeqCst) {
                                return;
                            }
                        }
                    }
                }
                drop(subscription);
                let delay = backoff.next_delay(jitter_random);
                let deadline = std::time::Instant::now() + Duration::from_secs_f64(delay);
                while std::time::Instant::now() < deadline {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })
    }

    // MARK: - Event handling

    fn handle(&mut self, event: LoopEvent) {
        let now = self.now();
        match event {
            LoopEvent::Input(bytes) => {
                self.input_generation += 1;
                for input in self.parser.feed(&bytes) {
                    self.apply(&input, now);
                }
                if self.parser.has_pending_input() {
                    // Give the rest of an escape sequence a moment before treating ESC as a key;
                    // half-received sequences (mouse reports, pastes) get a longer grace period.
                    let generation = self.input_generation;
                    let delay = Duration::from_secs_f64(self.parser.flush_delay());
                    let sender = self.sender.clone();
                    if let Err(error) = spawn("dtop-flush", move || {
                        std::thread::sleep(delay);
                        let _ = sender.send(LoopEvent::FlushInput(generation));
                    }) {
                        self.state.post(
                            format!("{error} — check system thread limits"),
                            NoticeLevel::Error,
                            now,
                            Some(8.0),
                        );
                        for input in self.parser.flush() {
                            self.apply(&input, now);
                        }
                    }
                }
            }
            LoopEvent::FlushInput(generation) => {
                if generation == self.input_generation {
                    for input in self.parser.flush() {
                        self.apply(&input, now);
                    }
                }
            }
            LoopEvent::InputClosed | LoopEvent::Terminate => self.state.should_quit = true,
            LoopEvent::Tick => {
                self.tick_count += 1;
                if self.state.is_paused {
                    return;
                }
                for index in 0..self.sessions.len() {
                    let connection = &self.sessions[index].snapshot.connection;
                    if !(connection.is_connected() || *connection == ConnectionState::Connecting) {
                        continue;
                    }
                    // Background hosts only need their container list; inventory every fifth tick.
                    let inventory = self.tick_count % 5 == 0
                        || (index == self.active() && self.state.tab.uses_inventory());
                    self.request_refresh(index, inventory);
                }
                self.load_detail_if_needed(true);
            }
            LoopEvent::FastTick => {
                self.state.expire_notice(now);
                let current = self.terminal.size(&self.environment);
                if current != self.size {
                    self.size = current;
                    self.renderer.invalidate();
                }
                if self.state.is_paused {
                    return;
                }
                for index in 0..self.sessions.len() {
                    if let ConnectionState::Disconnected {
                        retry_at: Some(at), ..
                    } = self.sessions[index].snapshot.connection
                    {
                        if now >= at {
                            self.request_refresh(index, true);
                        }
                    }
                    if self.sessions[index].event_refresh_due {
                        self.sessions[index].event_refresh_due = false;
                        self.request_refresh(index, true);
                    }
                }
                if self
                    .log_restart
                    .as_ref()
                    .is_some_and(|retry| now >= retry.at)
                {
                    self.log_restart = None;
                    self.update_log_stream(true);
                }
            }
            LoopEvent::DockerEvent(host, event) => {
                if event.affects_inventory() {
                    if let Some(session) = self.sessions.get_mut(host) {
                        session.event_refresh_due = true;
                    }
                }
            }
            LoopEvent::Refreshed(host, snapshot) => {
                let Some(session) = self.sessions.get_mut(host) else {
                    return;
                };
                session.is_refreshing = false;
                let mut snapshot = *snapshot;
                // The controller owns the detail pane (it merges streamed logs into it).
                snapshot.detail = session.snapshot.detail.take();
                session.snapshot = snapshot;
                let pending = session.pending_refresh.take();
                if host == self.active() {
                    let snapshot = self.sessions[host].snapshot.clone();
                    self.state.reconcile_selection(&snapshot);
                    self.load_detail_if_needed(false);
                }
                if let Some(inventory) = pending {
                    self.request_refresh(host, inventory);
                }
            }
            LoopEvent::DetailLoaded {
                generation,
                host,
                pane,
                data,
            } => {
                if generation != self.detail_generation {
                    return;
                }
                self.is_loading_detail = false;
                if host != self.active() || pane != self.state.detail_pane {
                    self.load_detail_if_needed(false);
                    return;
                }
                let mut data = *data;
                let selected = list::selected_item(self.snapshot(), &self.state)
                    .and_then(|i| i.container().map(|c| c.id.clone()));
                if selected.as_deref() == Some(data.container_id.as_str()) {
                    if let Some(current) = self
                        .snapshot()
                        .detail
                        .as_ref()
                        .filter(|d| d.container_id == data.container_id)
                    {
                        // Keep the followed log stream's lines and status.
                        data.logs = current.logs.clone();
                        data.log_state = current.log_state.clone();
                    }
                    self.snapshot_mut().detail = Some(data);
                }
                self.load_detail_if_needed(false);
            }
            LoopEvent::OperationFinished(host, result) => {
                let (level, duration) = if result.succeeded {
                    (NoticeLevel::Success, 4.0)
                } else {
                    (NoticeLevel::Error, 8.0)
                };
                self.state.post(result.message, level, now, Some(duration));
                self.request_refresh(host, true);
                self.load_detail_if_needed(true);
            }
            LoopEvent::LogLines {
                generation,
                host,
                container_id,
                lines,
            } => {
                let matches = self.log_stream.as_ref().is_some_and(|s| {
                    s.generation == generation && s.host == host && s.container_id == container_id
                }) && host == self.active()
                    && self
                        .snapshot()
                        .detail
                        .as_ref()
                        .is_some_and(|d| d.container_id == container_id);
                if !matches {
                    return;
                }
                if self.state.is_paused {
                    // Paused: the screen stays frozen; keep the lines for when updates resume.
                    self.paused_log_lines.extend(lines);
                    let overflow = self
                        .paused_log_lines
                        .len()
                        .saturating_sub(ContainerDetailData::MAXIMUM_LOG_LINES);
                    self.paused_log_lines.drain(..overflow);
                    return;
                }
                self.append_streamed_logs(lines);
            }
            LoopEvent::LogStreamEnded {
                generation,
                host,
                container_id,
                error,
            } => {
                if !self.log_stream.as_ref().is_some_and(|s| {
                    s.generation == generation && s.host == host && s.container_id == container_id
                }) {
                    return;
                }
                self.log_stream = None;
                if host == self.active() {
                    if let Some(detail) = self
                        .snapshot_mut()
                        .detail
                        .as_mut()
                        .filter(|d| d.container_id == container_id)
                    {
                        detail.log_state = LogStreamState::Ended(error.clone());
                    }
                    // A running container's stream only ends on a hiccup: reconnect shortly.
                    if self
                        .snapshot()
                        .container(&container_id)
                        .is_some_and(|c| c.state.is_active())
                    {
                        self.log_restart = Some(LogRestart {
                            host,
                            container_id,
                            at: now + if error.is_none() { 1.0 } else { 3.0 },
                        });
                    }
                }
            }
        }
    }

    /// Adds streamed lines to the detail pane. When scrolled back, the same lines stay on screen.
    fn append_streamed_logs(&mut self, lines: Vec<LogLine>) {
        if lines.is_empty() {
            return;
        }
        let appended = lines.len();
        let Some(detail) = self.snapshot_mut().detail.as_mut() else {
            return;
        };
        detail.append_logs(lines);
        let kept = detail.logs.len();
        // Keep the same lines on screen while scrolled back; never past the oldest kept line.
        if !self.state.log_follow {
            self.state.detail_scroll =
                log_policy::scroll_back(self.state.detail_scroll, appended, kept);
        }
    }

    fn apply(&mut self, event: &InputEvent, now: f64) {
        let previous_selection = list::selected_item(self.snapshot(), &self.state).map(|i| i.id());
        let previous_pane = self.state.detail_pane;
        let snapshot = self.snapshot().clone();
        let commands = self
            .state
            .handle(event, &snapshot, &self.hit_map, now, &self.theme_names);
        for command in commands {
            self.execute(command);
        }
        let selection = list::selected_item(self.snapshot(), &self.state).map(|i| i.id());
        if selection != previous_selection || self.state.detail_pane != previous_pane {
            self.load_detail_if_needed(false);
        }
        self.update_log_stream(false);
        if !self.state.is_paused && !self.paused_log_lines.is_empty() {
            let lines = std::mem::take(&mut self.paused_log_lines);
            self.append_streamed_logs(lines);
        }
    }

    fn execute(&mut self, command: AppCommand) {
        let now = self.now();
        match command {
            AppCommand::Quit => self.state.should_quit = true,
            AppCommand::Refresh { inventory } => self.request_refresh(self.active(), inventory),
            AppCommand::Perform(operation) => {
                let host = self.active();
                let refresher = Arc::clone(&self.sessions[host].connection.refresher);
                let sender = self.sender.clone();
                if let Err(error) = spawn("dtop-operation", move || {
                    let result = refresher.perform(&operation);
                    let _ = sender.send(LoopEvent::OperationFinished(host, result));
                }) {
                    self.state.post(
                        format!("{error} — the operation was not started"),
                        NoticeLevel::Error,
                        now,
                        Some(8.0),
                    );
                }
            }
            AppCommand::SwitchHost(_) => {
                // The reducer already moved `active_host`; show that host's data.
                self.detail_key = None;
                self.detail_generation = self.detail_generation.wrapping_add(1);
                self.is_loading_detail = false;
                let snapshot = self.snapshot().clone();
                self.state.reconcile_selection(&snapshot);
                if !snapshot.connection.is_connected()
                    || snapshot.last_updated.is_none()
                    || self.state.tab.uses_inventory()
                {
                    self.request_refresh(self.active(), true);
                }
                self.load_detail_if_needed(false);
            }
            AppCommand::OpenThemeEditor => {
                // Custom themes that fail to parse are skipped here; `t` reports their errors.
                let mut themes: Vec<Theme> = self
                    .theme_names
                    .iter()
                    .filter_map(|n| self.theme_loader.load(n, &read_text_file).ok())
                    .collect();
                if themes.is_empty() {
                    themes = Theme::built_in();
                }
                self.state.open_theme_editor(self.theme.clone(), themes);
            }
            AppCommand::SaveTheme { name, theme } => {
                match self.theme_store.save(&theme, &name, &LocalFileSystem) {
                    Ok(result) => {
                        self.theme = *theme;
                        if !self.theme_names.contains(&name) {
                            self.theme_names.push(name.clone());
                        }
                        let location = result.theme_file.unwrap_or(result.config_file);
                        self.state
                            .finish_theme_save(&name, None, Some(location), now);
                    }
                    Err(error) => {
                        self.state
                            .finish_theme_save(&name, Some(error.to_string()), None, now)
                    }
                }
            }
            AppCommand::ThemeChanged(name) => {
                match self.theme_loader.load(&name, &read_text_file) {
                    Ok(theme) => {
                        self.theme = theme;
                        self.renderer.invalidate();
                    }
                    Err(error) => {
                        self.state
                            .post(error.to_string(), NoticeLevel::Error, now, Some(6.0))
                    }
                }
            }
        }
    }

    fn request_refresh(&mut self, host: usize, inventory: bool) {
        let Some(session) = self.sessions.get_mut(host) else {
            return;
        };
        if session.is_refreshing {
            session.pending_refresh = Some(session.pending_refresh.unwrap_or(false) || inventory);
            return;
        }
        session.is_refreshing = true;
        let refresher = Arc::clone(&session.connection.refresher);
        let sender = self.sender.clone();
        if let Err(error) = spawn("dtop-refresh", move || {
            let snapshot = refresher.refresh(inventory);
            let _ = sender.send(LoopEvent::Refreshed(host, Box::new(snapshot)));
        }) {
            let now = self.now();
            let message = format!("{error} — check system thread limits");
            let session = &mut self.sessions[host];
            session.is_refreshing = false;
            session.snapshot.connection = ConnectionState::Disconnected {
                message: message.clone(),
                retry_at: Some(now + 3.0),
            };
            if host == self.active() {
                self.state.post(message, NoticeLevel::Error, now, Some(8.0));
            }
        }
    }

    fn details_visible(&self) -> bool {
        ScreenLayout::compute(self.size, true).is_some_and(|l| l.detail.is_some())
    }

    /// Loads detail data for the selected container when the pane is visible. The logs pane is
    /// fed by the followed stream instead.
    fn load_detail_if_needed(&mut self, force: bool) {
        self.update_log_stream(false);
        if !self.state.show_details
            || self.state.detail_pane == DetailPane::Logs
            || !self.details_visible()
        {
            return;
        }
        let Some(container) =
            list::selected_item(self.snapshot(), &self.state).and_then(|i| i.container().cloned())
        else {
            return;
        };
        let pane = self.state.detail_pane;
        let key = format!("{}:{}", container.id, pane.title());
        let now = self.now();
        // Overview data (inspect) changes rarely; processes refresh with every tick.
        if force
            && self.detail_key.as_deref() == Some(key.as_str())
            && pane == DetailPane::Overview
            && now - self.last_detail_load < 10.0
        {
            return;
        }
        if self.detail_key.as_deref() == Some(key.as_str()) && (self.is_loading_detail || !force) {
            return;
        }
        self.detail_key = Some(key);
        self.is_loading_detail = true;
        self.detail_generation = self.detail_generation.wrapping_add(1);
        let generation = self.detail_generation;
        self.last_detail_load = now;
        let host = self.active();
        let refresher = Arc::clone(&self.sessions[host].connection.refresher);
        let sender = self.sender.clone();
        let is_active = container.state.is_active();
        if let Err(error) = spawn("dtop-detail", move || {
            let data = refresher.load_detail(&container.id, pane, is_active);
            let _ = sender.send(LoopEvent::DetailLoaded {
                generation,
                host,
                pane,
                data: Box::new(data),
            });
        }) {
            self.is_loading_detail = false;
            self.detail_key = None;
            self.state.post(
                format!("{error} — check system thread limits"),
                NoticeLevel::Error,
                now,
                Some(8.0),
            );
        }
    }

    /// Which container's logs should stream right now, if any.
    fn wanted_log_stream(&self) -> Option<Target> {
        if !self.state.show_details
            || self.state.detail_pane != DetailPane::Logs
            || !self.details_visible()
        {
            return None;
        }
        let container = list::selected_item(self.snapshot(), &self.state)?
            .container()?
            .clone();
        Some(Target {
            host: self.active(),
            container_id: container.id.clone(),
            is_active: container.state.is_active(),
        })
    }

    /// Starts, keeps or stops the followed log stream to match the selection. `restart` is a
    /// scheduled reconnect for a running container whose stream dropped.
    fn update_log_stream(&mut self, restart: bool) {
        let wanted = self.wanted_log_stream();
        if self.log_restart.as_ref().is_some_and(|retry| {
            wanted.as_ref().map_or(true, |target| {
                retry.host != target.host || retry.container_id != target.container_id
            })
        }) {
            self.log_restart = None;
            self.paused_log_lines.clear();
        }
        if wanted.is_none() {
            self.paused_log_lines.clear();
        }
        let existing = wanted.as_ref().and_then(|t| {
            self.snapshot()
                .detail
                .clone()
                .filter(|d| d.container_id == t.container_id)
        });
        let current = self
            .log_stream
            .as_ref()
            .map(|s| (s.host, s.container_id.as_str()));
        let decision = log_policy::decide(
            current,
            wanted.as_ref(),
            existing.as_ref().map(|d| &d.log_state),
            self.log_restart.is_some(),
            restart,
        );
        match decision {
            Decision::Keep | Decision::Wait => return,
            Decision::Stop | Decision::Start => {
                if let Some(stream) = self.log_stream.take() {
                    stream.cancel();
                }
                self.log_restart = None;
                self.paused_log_lines.clear();
                if decision == Decision::Stop {
                    return;
                }
            }
        }
        let Some(wanted) = wanted else { return };
        let mut detail = existing.unwrap_or_else(|| ContainerDetailData::new(&wanted.container_id));
        // Same container as before (a reconnect, or it started again): keep what is on screen and
        // resume from the last line's timestamp instead of re-reading the tail.
        let resume_after = detail.logs.last().and_then(|l| l.timestamp);
        if resume_after.is_none() {
            detail.logs.clear();
            self.state.detail_scroll = 0;
            self.state.log_follow = true;
        }
        detail.log_state = LogStreamState::Live;
        self.snapshot_mut().detail = Some(detail);
        let api = Arc::clone(&self.sessions[wanted.host].connection.api);
        let subscription = api.follow_logs(&wanted.container_id, self.log_tail, resume_after);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.log_generation = self.log_generation.wrapping_add(1);
        let generation = self.log_generation;
        let stream = LogStream {
            generation,
            host: wanted.host,
            container_id: wanted.container_id.clone(),
            cancelled: Arc::clone(&cancelled),
            cancellation: subscription.cancellation(),
        };
        let sender = self.sender.clone();
        let (host, container_id) = (wanted.host, wanted.container_id);
        let error_container_id = container_id.clone();
        self.log_stream = Some(stream);
        if let Err(error) = spawn("dtop-logs-forward", move || {
            let error = loop {
                match subscription.receiver.recv() {
                    Ok(StreamItem::Item(batch)) => {
                        // `since` is inclusive: drop lines already shown.
                        let fresh = log_policy::new_lines(batch, resume_after);
                        if !fresh.is_empty() && !cancelled.load(Ordering::SeqCst) {
                            let event = LoopEvent::LogLines {
                                generation,
                                host,
                                container_id: container_id.clone(),
                                lines: fresh,
                            };
                            if sender.send(event).is_err() {
                                return;
                            }
                        }
                    }
                    Ok(StreamItem::End) | Err(_) => break None,
                    Ok(StreamItem::Failed(e)) => break Some(describe(&e)),
                }
            };
            if !cancelled.load(Ordering::SeqCst) {
                let _ = sender.send(LoopEvent::LogStreamEnded {
                    generation,
                    host,
                    container_id,
                    error,
                });
            }
        }) {
            self.handle(LoopEvent::LogStreamEnded {
                generation,
                host,
                container_id: error_container_id,
                error: Some(format!("{error} — check system thread limits")),
            });
        }
    }

    // MARK: - Rendering

    fn render(&mut self) {
        let hosts: Vec<HostSummary> = self
            .sessions
            .iter()
            .map(|s| {
                HostSummary::new(
                    &s.connection.host.name,
                    &s.connection.host.description(),
                    &s.snapshot,
                )
            })
            .collect();
        let snapshot = &self.sessions[self.state.active_host].snapshot;
        let context = RenderContext {
            state: &self.state,
            snapshot,
            theme: &self.theme,
            size: self.size,
            clock: local_clock(),
            now: self.now(),
            hosts: &hosts,
        };
        let frame = views::render(&context);
        self.hit_map = frame.hit_map;
        self.state
            .scroll_offsets
            .insert(self.state.tab, self.hit_map.list_scroll_offset);
        let output = self.renderer.render(frame.buffer);
        self.terminal.write(&output);
    }
}

// MARK: - Signals and panics

#[cfg(unix)]
fn install_signal_handlers() {
    extern "C" fn on_signal(_: std::ffi::c_int) {
        TERMINATE_REQUESTED.store(true, Ordering::SeqCst);
    }
    extern "C" {
        fn signal(signum: std::ffi::c_int, handler: extern "C" fn(std::ffi::c_int)) -> usize;
    }
    // SIGHUP, SIGINT, SIGTERM have the same numbers on Linux and macOS.
    for signum in [1, 2, 15] {
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe {
            signal(signum, on_signal);
        }
    }
}

#[cfg(not(unix))]
fn install_signal_handlers() {}

/// Restores the terminal before the panic message prints, so it is readable.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        dtop_ui::terminal::write_raw_stdout(dtop_ui::terminal::leave_sequence().as_bytes());
        dtop_ui::terminal::restore_saved_mode();
        previous(info);
    }));
}

// MARK: - Local wall clock

/// `HH:mm:ss` in the local time zone.
pub fn local_clock() -> String {
    let (h, m, s) = local_time();
    format!("{h:02}:{m:02}:{s:02}")
}

#[cfg(unix)]
fn local_time() -> (i32, i32, i32) {
    #[repr(C)]
    struct Tm {
        tm_sec: i32,
        tm_min: i32,
        tm_hour: i32,
        tm_mday: i32,
        tm_mon: i32,
        tm_year: i32,
        tm_wday: i32,
        tm_yday: i32,
        tm_isdst: i32,
        tm_gmtoff: std::ffi::c_long,
        tm_zone: *const std::ffi::c_char,
    }
    extern "C" {
        fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
    }
    let now = format::now() as i64;
    // SAFETY: Tm matches `struct tm` on Linux (glibc, musl) and macOS; localtime_r fills it.
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        if localtime_r(&now, &mut tm).is_null() {
            return (0, 0, 0);
        }
        (tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

#[cfg(windows)]
fn local_time() -> (i32, i32, i32) {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(time: *mut SystemTime);
    }
    let mut time = SystemTime::default();
    // SAFETY: GetLocalTime fills a SYSTEMTIME.
    unsafe { GetLocalTime(&mut time) };
    (
        i32::from(time.hour),
        i32::from(time.minute),
        i32::from(time.second),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtop_core::logstream::LogStreamKind;
    use dtop_core::models::{Container, ContainerState};
    use dtop_core::platform::Platform;
    use dtop_core::tls::client_for_host;
    use dtop_core::DockerEndpoint;
    use dtop_ui::{Key, KeyEvent};

    fn controller() -> AppController {
        let hosts = ["first", "second"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let host = DockerHost::new(
                    name,
                    DockerEndpoint::Tcp {
                        host: "127.0.0.1".into(),
                        port: 1 + index as u16,
                    },
                );
                let api: Arc<dyn DockerApi> = Arc::new(client_for_host(&host, 1).unwrap());
                let refresher = Arc::new(DataRefresher::new(
                    Arc::clone(&api),
                    &host.description(),
                    crate::refresher::RefresherOptions::default(),
                ));
                HostConnection {
                    host,
                    api,
                    refresher,
                }
            })
            .collect();
        let mut controller = AppController::new(ControllerOptions {
            configuration: AppConfiguration::default(),
            hosts,
            notice: None,
            theme: Theme::named("harbor").unwrap(),
            color_mode: ColorMode::None,
            theme_loader: ThemeLoader {
                themes_directory: None,
                platform: Platform::current(),
            },
            theme_names: Vec::new(),
            theme_store: ThemeStore::new(None, None, Platform::current()),
            environment: HashMap::new(),
        });
        controller.size = TerminalSize {
            columns: 150,
            rows: 40,
        };
        for (index, session) in controller.sessions.iter_mut().enumerate() {
            let id = format!("container-{index}");
            session.snapshot.containers =
                vec![Container::new(&id, &id, "alpine", ContainerState::Exited)];
            let mut detail = ContainerDetailData::new(&id);
            detail.log_state = LogStreamState::Live;
            session.snapshot.detail = Some(detail);
        }
        controller.state.detail_pane = DetailPane::Logs;
        controller.log_stream = Some(LogStream {
            generation: 1,
            host: 0,
            container_id: "container-0".into(),
            cancelled: Arc::new(AtomicBool::new(false)),
            cancellation: Arc::new(StreamCancellation::default()),
        });
        controller
    }

    #[test]
    fn resuming_buffered_logs_preserves_the_ended_stream_status() {
        let mut controller = controller();
        controller.state.is_paused = true;
        controller.handle(LoopEvent::LogLines {
            generation: 1,
            host: 0,
            container_id: "container-0".into(),
            lines: vec![LogLine::new(LogStreamKind::Stdout, "last line")],
        });
        controller.handle(LoopEvent::LogStreamEnded {
            generation: 1,
            host: 0,
            container_id: "container-0".into(),
            error: None,
        });
        controller.apply(&InputEvent::Key(KeyEvent::new(Key::Char(' '))), 1.0);
        let detail = controller.snapshot().detail.as_ref().unwrap();
        assert_eq!(detail.logs[0].text, "last line");
        assert_eq!(detail.log_state, LogStreamState::Ended(None));
    }

    #[test]
    fn buffered_logs_are_not_added_to_a_different_host_after_hiding_the_pane() {
        let mut controller = controller();
        controller.state.is_paused = true;
        controller.handle(LoopEvent::LogLines {
            generation: 1,
            host: 0,
            container_id: "container-0".into(),
            lines: vec![LogLine::new(LogStreamKind::Stdout, "first host only")],
        });
        controller.state.detail_pane = DetailPane::Overview;
        controller.update_log_stream(false);
        controller.state.switch_host(1);
        controller.apply(&InputEvent::Key(KeyEvent::new(Key::Char(' '))), 1.0);
        assert!(controller
            .snapshot()
            .detail
            .as_ref()
            .unwrap()
            .logs
            .is_empty());
    }

    #[test]
    fn stale_log_events_cannot_end_or_modify_a_new_stream_for_the_same_container() {
        let mut controller = controller();
        controller.log_stream.as_mut().unwrap().generation = 2;
        controller.handle(LoopEvent::LogLines {
            generation: 1,
            host: 0,
            container_id: "container-0".into(),
            lines: vec![LogLine::new(LogStreamKind::Stdout, "stale")],
        });
        controller.handle(LoopEvent::LogStreamEnded {
            generation: 1,
            host: 0,
            container_id: "container-0".into(),
            error: Some("old connection closed".into()),
        });
        assert_eq!(controller.log_stream.as_ref().unwrap().generation, 2);
        assert!(controller
            .snapshot()
            .detail
            .as_ref()
            .unwrap()
            .logs
            .is_empty());
        controller.handle(LoopEvent::LogLines {
            generation: 2,
            host: 0,
            container_id: "container-0".into(),
            lines: vec![LogLine::new(LogStreamKind::Stdout, "current")],
        });
        assert_eq!(
            controller.snapshot().detail.as_ref().unwrap().logs[0].text,
            "current"
        );
    }

    #[test]
    fn switching_hosts_cancels_the_previous_containers_log_retry() {
        let mut controller = controller();
        controller.sessions[0].snapshot.containers[0].state = ContainerState::Running;
        controller.handle(LoopEvent::LogStreamEnded {
            generation: 1,
            host: 0,
            container_id: "container-0".into(),
            error: Some("connection lost".into()),
        });
        assert!(controller.log_restart.is_some());
        controller.sessions[1]
            .snapshot
            .detail
            .as_mut()
            .unwrap()
            .log_state = LogStreamState::Ended(None);
        controller.state.switch_host(1);
        controller.update_log_stream(false);
        assert!(controller.log_restart.is_none());
        assert!(controller.log_stream.is_none());
    }

    #[test]
    fn stale_detail_results_cannot_replace_a_newer_request() {
        let mut controller = controller();
        controller.state.detail_pane = DetailPane::Overview;
        controller.detail_key = Some("container-0:Overview".into());
        controller.detail_generation = 2;
        controller.is_loading_detail = true;
        let mut stale = ContainerDetailData::new("container-0");
        stale.error = Some("old request".into());
        controller.handle(LoopEvent::DetailLoaded {
            generation: 1,
            host: 0,
            pane: DetailPane::Overview,
            data: Box::new(stale),
        });
        assert!(controller.is_loading_detail);
        assert!(controller
            .snapshot()
            .detail
            .as_ref()
            .unwrap()
            .error
            .is_none());
        let mut current = ContainerDetailData::new("container-0");
        current.error = Some("current request".into());
        controller.handle(LoopEvent::DetailLoaded {
            generation: 2,
            host: 0,
            pane: DetailPane::Overview,
            data: Box::new(current),
        });
        assert!(!controller.is_loading_detail);
        assert_eq!(
            controller
                .snapshot()
                .detail
                .as_ref()
                .unwrap()
                .error
                .as_deref(),
            Some("current request")
        );
    }

    #[test]
    fn changing_selection_starts_a_new_detail_load_without_waiting_for_the_old_one() {
        let mut controller = controller();
        controller.state.detail_pane = DetailPane::Overview;
        controller.detail_key = Some("other-container:Overview".into());
        controller.detail_generation = 1;
        controller.is_loading_detail = true;
        controller.load_detail_if_needed(false);
        assert_eq!(controller.detail_generation, 2);
        assert_eq!(
            controller.detail_key.as_deref(),
            Some("container-0:Overview")
        );
    }
}
