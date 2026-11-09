//! App-layer tests: reducer, list model, views, refresher, multi-host and log streaming, with a
//! fake Docker API (no daemon or terminal needed).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dtop_app::config::AppConfiguration;
use dtop_app::list::{self, ListItem};
use dtop_app::log_policy;
use dtop_app::once;
use dtop_app::operation::{DockerOperation, PruneTarget};
use dtop_app::reducer::{AppCommand, HitMap};
use dtop_app::refresher::{DataRefresher, RefresherOptions};
use dtop_app::state::*;
use dtop_app::views::{self, RenderContext, ScreenLayout};
use dtop_core::compose::{CommandInvocation, CommandResult, CommandRunning, ComposeOperation};
use dtop_core::logstream::{LogLine, LogStreamKind};
use dtop_core::models::*;
use dtop_core::monitoring::{Backoff, ResourceSample};
use dtop_core::{ApiVersion, ContainerAction, DockerApi, DockerError, Result, Subscription};
use dtop_ui::buffer::Rect;
use dtop_ui::render::FrameRenderer;
use dtop_ui::{
    ColorMode, InputEvent, Key, KeyEvent, Modifiers, MouseAction, MouseButton, MouseEvent,
    TerminalSize, Theme,
};

// MARK: - Fakes

#[derive(Default)]
struct FakeApi {
    containers: Mutex<Vec<Container>>,
    fail_list: Mutex<Option<DockerError>>,
    fail_volumes: bool,
    stats_calls: AtomicUsize,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    actions: Mutex<Vec<(String, String)>>,
    fail_action_for: Option<String>,
}

fn stats_json(total: u64, system: u64) -> ContainerStats {
    let text = format!(
        r#"{{"read":"2024-01-01T00:00:0{}Z","cpu_stats":{{"cpu_usage":{{"total_usage":{total}}},"system_cpu_usage":{system},"online_cpus":2}},"memory_stats":{{"usage":1048576,"limit":4194304}}}}"#,
        total % 10
    );
    ContainerStats::decode(&dtop_core::json::parse_str(&text).unwrap())
}

impl DockerApi for FakeApi {
    fn ping(&self) -> Result<Option<ApiVersion>> {
        Ok(ApiVersion::parse("1.45"))
    }
    fn version(&self) -> Result<DockerVersion> {
        Ok(DockerVersion {
            version: "27.0.0".into(),
            api_version: "1.45".into(),
            ..Default::default()
        })
    }
    fn info(&self) -> Result<SystemInfo> {
        Ok(SystemInfo {
            name: "box".into(),
            cpu_count: 4,
            memory_total: 8 << 30,
            ..Default::default()
        })
    }
    fn list_containers(&self, _all: bool) -> Result<Vec<Container>> {
        if let Some(e) = self.fail_list.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(self.containers.lock().unwrap().clone())
    }
    fn inspect_container(&self, id: &str) -> Result<ContainerDetails> {
        if id == "broken" {
            return Err(DockerError::Http {
                status: 404,
                message: "No such container: broken".into(),
            });
        }
        Ok(ContainerDetails {
            id: id.into(),
            path: "nginx".into(),
            args: vec!["-g".into()],
            ..Default::default()
        })
    }
    fn stats(&self, id: &str) -> Result<ContainerStats> {
        if id == "panic" {
            panic!("simulated stats worker failure");
        }
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(5));
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        let n = self.stats_calls.fetch_add(1, Ordering::SeqCst) as u64;
        if id == "flaky" {
            return Err(DockerError::Http {
                status: 409,
                message: "stopped".into(),
            });
        }
        Ok(stats_json(1_000 * (n + 1), 10_000 * (n + 1)))
    }
    fn top(&self, _id: &str) -> Result<ContainerTop> {
        Ok(ContainerTop {
            titles: vec![
                "UID".into(),
                "PID".into(),
                "PPID".into(),
                "C".into(),
                "CMD".into(),
            ],
            processes: vec![vec![
                "root".into(),
                "1".into(),
                "0".into(),
                "0".into(),
                "init".into(),
            ]],
        })
    }
    fn logs(&self, _id: &str, _tail: usize) -> Result<Vec<LogLine>> {
        Ok(vec![
            LogLine::new(LogStreamKind::Stdout, "hello"),
            LogLine::new(LogStreamKind::Stderr, "oops"),
        ])
    }
    fn perform(&self, action: &ContainerAction, id: &str) -> Result<()> {
        if self.fail_action_for.as_deref() == Some(id) {
            return Err(DockerError::Http {
                status: 500,
                message: "boom".into(),
            });
        }
        self.actions
            .lock()
            .unwrap()
            .push((action.verb().into(), id.into()));
        Ok(())
    }
    fn remove_container(&self, id: &str, _force: bool, _v: bool) -> Result<()> {
        self.actions
            .lock()
            .unwrap()
            .push(("remove".into(), id.into()));
        Ok(())
    }
    fn prune_containers(&self) -> Result<PruneReport> {
        Ok(PruneReport {
            deleted: vec!["a".into(), "b".into()],
            space_reclaimed: 2048,
        })
    }
    fn list_images(&self) -> Result<Vec<DockerImage>> {
        Ok(vec![
            DockerImage::new("sha256:abc", &["nginx:latest"], 1 << 20),
            DockerImage::new("sha256:def", &[], 10),
        ])
    }
    fn remove_image(&self, _id: &str, _force: bool) -> Result<()> {
        Ok(())
    }
    fn prune_images(&self, _d: bool) -> Result<PruneReport> {
        Ok(PruneReport::default())
    }
    fn list_networks(&self) -> Result<Vec<DockerNetwork>> {
        Ok(vec![
            DockerNetwork::new("n1", "bridge"),
            DockerNetwork::new("n2", "app_default"),
        ])
    }
    fn remove_network(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    fn prune_networks(&self) -> Result<PruneReport> {
        Ok(PruneReport::default())
    }
    fn list_volumes(&self) -> Result<Vec<DockerVolume>> {
        if self.fail_volumes {
            return Err(DockerError::Http {
                status: 500,
                message: "volumes broke".into(),
            });
        }
        Ok(vec![
            DockerVolume::new("data"),
            DockerVolume::new(&"a".repeat(64)),
        ])
    }
    fn remove_volume(&self, _n: &str, _f: bool) -> Result<()> {
        Ok(())
    }
    fn prune_volumes(&self) -> Result<PruneReport> {
        Ok(PruneReport::default())
    }
    fn events(&self) -> Subscription<DockerEvent> {
        Subscription::from_items(Vec::new(), None)
    }
}

struct FakeRunner(Mutex<Vec<CommandInvocation>>, i32);

impl CommandRunning for FakeRunner {
    fn run(&self, invocation: &CommandInvocation) -> Result<CommandResult> {
        self.0.lock().unwrap().push(invocation.clone());
        Ok(CommandResult {
            exit_code: self.1,
            output: "line one\nlast line".into(),
        })
    }
}

fn container(id: &str, name: &str, state: ContainerState) -> Container {
    let mut c = Container::new(id, name, "img:1", state);
    c.status = "Up 3 minutes".into();
    c
}

fn compose(id: &str, name: &str, project: &str, service: &str, state: ContainerState) -> Container {
    let mut c = container(id, name, state);
    c.labels
        .insert(compose_label::PROJECT.into(), project.into());
    c.labels
        .insert(compose_label::SERVICE.into(), service.into());
    c.labels
        .insert(compose_label::WORKING_DIR.into(), "/srv/app".into());
    c.labels.insert(
        compose_label::CONFIG_FILES.into(),
        "/srv/app/compose.yaml".into(),
    );
    c
}

fn sample(cpu: f64, memory: u64) -> ResourceSample {
    ResourceSample {
        cpu_percent: cpu,
        memory_usage: memory,
        memory_limit: 1 << 30,
        cpu_count: 4,
        ..Default::default()
    }
}

fn snapshot() -> DockerSnapshot {
    let mut s = DockerSnapshot {
        endpoint: "unix:///var/run/docker.sock".into(),
        connection: ConnectionState::Connected,
        version: Some(DockerVersion {
            version: "27.0.0".into(),
            ..Default::default()
        }),
        info: Some(SystemInfo {
            name: "box".into(),
            cpu_count: 4,
            memory_total: 8 << 30,
            ..Default::default()
        }),
        containers: vec![
            container("aaa", "web", ContainerState::Running),
            container("bbb", "db", ContainerState::Running),
            container("ccc", "old", ContainerState::Exited),
            compose("ddd", "app-api-1", "app", "api", ContainerState::Running),
            compose(
                "eee",
                "app-worker-1",
                "app",
                "worker",
                ContainerState::Paused,
            ),
        ],
        images: vec![DockerImage::new("sha256:abc", &["nginx:latest"], 1 << 20)],
        networks: vec![
            DockerNetwork::new("n1", "bridge"),
            DockerNetwork::new("n2", "app_default"),
        ],
        volumes: vec![DockerVolume::new("data")],
        last_updated: Some(1.0),
        ..DockerSnapshot::default()
    };
    let mut samples = BTreeMap::new();
    samples.insert("aaa".to_string(), sample(50.0, 100 << 20));
    samples.insert("bbb".to_string(), sample(5.0, 500 << 20));
    samples.insert("ddd".to_string(), sample(20.0, 10 << 20));
    s.metrics.record(samples, 1.0, None);
    s
}

fn key(k: Key) -> InputEvent {
    InputEvent::Key(KeyEvent::new(k))
}

fn ch(c: char) -> InputEvent {
    key(Key::Char(c))
}

fn press(state: &mut AppState, snapshot: &DockerSnapshot, event: InputEvent) -> Vec<AppCommand> {
    state.handle(
        &event,
        snapshot,
        &HitMap::new(),
        100.0,
        &["harbor".to_string(), "ember".to_string()],
    )
}

fn names(items: &[ListItem]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i {
            ListItem::Container { container, .. } => container.name(),
            ListItem::ComposeProject { project, .. } => format!("[{}]", project.name),
            ListItem::Image(i) => i.display_name(),
            ListItem::Network(n) => n.name.clone(),
            ListItem::Volume(v) => v.name.clone(),
        })
        .collect()
}

fn render(
    state: &AppState,
    snapshot: &DockerSnapshot,
    columns: i32,
    rows: i32,
) -> views::RenderedFrame {
    render_with(state, snapshot, columns, rows, &[])
}

fn render_with(
    state: &AppState,
    snapshot: &DockerSnapshot,
    columns: i32,
    rows: i32,
    hosts: &[HostSummary],
) -> views::RenderedFrame {
    let theme = Theme::named(&state.theme_name).unwrap_or_else(Theme::harbor);
    let context = RenderContext {
        state,
        snapshot,
        theme: &theme,
        size: TerminalSize { columns, rows },
        clock: "12:34:56".into(),
        now: 100.0,
        hosts,
    };
    views::render(&context)
}

// MARK: - List model and reducer

#[test]
fn containers_sort_by_cpu_descending_then_state_then_name() {
    let state = AppState::default();
    let items = list::items(Tab::Containers, &snapshot(), &state);
    assert_eq!(
        names(&items),
        vec!["web", "app-api-1", "db", "app-worker-1", "old"]
    );
}

#[test]
fn every_sort_key_orders_containers() {
    let s = snapshot();
    for (key, descending, first) in [
        (SortKey::Name, false, "app-api-1"),
        (SortKey::Memory, true, "db"),
        (SortKey::Status, false, "app-api-1"),
        (SortKey::Image, false, "app-api-1"),
    ] {
        let state = AppState {
            sort_key: key,
            sort_descending: descending,
            ..AppState::default()
        };
        assert_eq!(
            names(&list::items(Tab::Containers, &s, &state))[0],
            first,
            "{key:?}"
        );
    }
}

#[test]
fn status_and_text_filters_combine() {
    let s = snapshot();
    let state = AppState {
        status_filter: StatusFilter::Stopped,
        ..AppState::default()
    };
    assert_eq!(
        names(&list::items(Tab::Containers, &s, &state)),
        vec!["old"]
    );
    let state = AppState {
        filter_text: "APP".into(),
        ..AppState::default()
    };
    assert_eq!(names(&list::items(Tab::Containers, &s, &state)).len(), 2);
}

#[test]
fn image_names_follow_the_sort_direction() {
    let s = DockerSnapshot {
        images: vec![
            DockerImage::new("sha256:b", &["beta:1"], 1),
            DockerImage::new("sha256:a", &["alpha:1"], 2),
        ],
        ..DockerSnapshot::default()
    };
    let mut state = AppState {
        sort_key: SortKey::Name,
        sort_descending: false,
        ..AppState::default()
    };
    assert_eq!(
        names(&list::items(Tab::Images, &s, &state)),
        vec!["alpha:1", "beta:1"]
    );
    state.sort_descending = true;
    assert_eq!(
        names(&list::items(Tab::Images, &s, &state)),
        vec!["beta:1", "alpha:1"]
    );
    // Keys that do not apply to images keep them A→Z whatever the direction.
    state.sort_key = SortKey::Cpu;
    assert_eq!(
        names(&list::items(Tab::Images, &s, &state)),
        vec!["alpha:1", "beta:1"]
    );
}

#[test]
fn compose_tab_nests_containers_and_honours_collapse() {
    let s = snapshot();
    let mut state = AppState::default();
    assert_eq!(
        names(&list::items(Tab::Compose, &s, &state)),
        vec!["[app]", "app-api-1", "app-worker-1"]
    );
    state.collapsed_projects.insert("app".into());
    assert_eq!(names(&list::items(Tab::Compose, &s, &state)), vec!["[app]"]);
}

#[test]
fn navigation_keys_move_within_bounds() {
    let s = snapshot();
    let mut state = AppState::default();
    for event in [key(Key::Down), ch('j'), ch('j')] {
        press(&mut state, &s, event);
    }
    assert_eq!(state.selected_indexes[&Tab::Containers], 3);
    press(&mut state, &s, ch('G'));
    assert_eq!(state.selected_indexes[&Tab::Containers], 4);
    press(&mut state, &s, key(Key::Down));
    assert_eq!(state.selected_indexes[&Tab::Containers], 4);
    press(&mut state, &s, ch('g'));
    assert_eq!(state.selected_indexes[&Tab::Containers], 0);
}

#[test]
fn tabs_cycle_and_jump_with_digits() {
    let s = snapshot();
    let mut state = AppState::default();
    press(&mut state, &s, key(Key::Tab));
    assert_eq!(state.tab, Tab::Compose);
    press(&mut state, &s, key(Key::Left));
    press(&mut state, &s, key(Key::Left));
    assert_eq!(state.tab, Tab::Volumes);
    press(&mut state, &s, ch('3'));
    assert_eq!(state.tab, Tab::Images);
}

#[test]
fn toggles_sort_filter_pause_and_theme() {
    let s = snapshot();
    let mut state = AppState::default();
    press(&mut state, &s, ch('s'));
    assert_eq!(
        (state.sort_key, state.sort_descending),
        (SortKey::Memory, true)
    );
    press(&mut state, &s, ch('a'));
    assert_eq!(state.status_filter, StatusFilter::Running);
    press(&mut state, &s, ch(' '));
    assert!(state.is_paused);
    assert_eq!(
        press(&mut state, &s, ch('t')),
        vec![AppCommand::ThemeChanged("ember".into())]
    );
    press(&mut state, &s, ch('v'));
    assert_eq!(state.detail_pane, DetailPane::Processes);
}

#[test]
fn ctrl_c_quits_from_every_mode() {
    let s = snapshot();
    for mode in [
        InputMode::Normal,
        InputMode::Filter,
        InputMode::Help,
        InputMode::HostPicker(0),
    ] {
        let mut state = AppState {
            mode,
            ..AppState::default()
        };
        assert_eq!(
            press(&mut state, &s, InputEvent::Key(KeyEvent::ctrl('c'))),
            vec![AppCommand::Quit]
        );
    }
}

#[test]
fn filter_mode_edits_text_and_paste_strips_newlines() {
    let s = snapshot();
    let mut state = AppState::default();
    press(&mut state, &s, ch('/'));
    for c in "we q".chars() {
        press(&mut state, &s, ch(c));
    }
    assert_eq!(state.filter_text, "we q");
    press(&mut state, &s, InputEvent::Key(KeyEvent::ctrl('w')));
    assert_eq!(state.filter_text, "we ");
    press(&mut state, &s, key(Key::Escape));
    assert_eq!(
        (state.filter_text.as_str(), &state.mode),
        ("", &InputMode::Normal)
    );
    press(&mut state, &s, InputEvent::Paste("db\n".into()));
    assert_eq!(
        (state.filter_text.as_str(), &state.mode),
        ("db", &InputMode::Filter)
    );
}

#[test]
fn safe_actions_run_and_destructive_ones_confirm() {
    let s = snapshot();
    let mut state = AppState::default();
    let commands = press(&mut state, &s, ch('r'));
    assert!(matches!(
        &commands[..],
        [AppCommand::Perform(DockerOperation::Container {
            action: ContainerAction::Restart { .. },
            ..
        })]
    ));
    assert!(press(&mut state, &s, ch('D')).is_empty());
    let InputMode::Confirm(confirmation) = &state.mode else {
        panic!("expected a confirmation")
    };
    assert!(!confirmation.confirm_focused);
    assert!(press(&mut state, &s, key(Key::Enter)).is_empty());
    assert_eq!(state.mode, InputMode::Normal);
    press(&mut state, &s, ch('D'));
    let commands = press(&mut state, &s, ch('y'));
    assert!(matches!(
        &commands[..],
        [AppCommand::Perform(DockerOperation::RemoveContainer {
            force: true,
            ..
        })]
    ));
}

#[test]
fn inapplicable_actions_explain_why() {
    let s = snapshot();
    let mut state = AppState::default();
    press(&mut state, &s, ch('u'));
    assert!(state
        .notice
        .as_ref()
        .unwrap()
        .text
        .contains("already running"));
    state.tab = Tab::Networks;
    press(&mut state, &s, ch('j'));
    press(&mut state, &s, ch('D'));
    assert!(state.notice.as_ref().unwrap().text.contains("built-in"));
    press(&mut state, &s, ch('X'));
    assert!(
        matches!(&state.mode, InputMode::Confirm(c) if c.operation == DockerOperation::Prune(PruneTarget::Networks))
    );
}

#[test]
fn compose_keys_map_to_project_operations() {
    let s = snapshot();
    let mut state = AppState {
        tab: Tab::Compose,
        ..AppState::default()
    };
    let commands = press(&mut state, &s, ch('U'));
    assert!(matches!(
        &commands[..],
        [AppCommand::Perform(DockerOperation::Compose {
            operation: ComposeOperation::Up,
            ..
        })]
    ));
    press(&mut state, &s, key(Key::Enter));
    assert!(state.collapsed_projects.contains("app"));
}

#[test]
fn mouse_clicks_tabs_rows_and_wheel_scrolls() {
    let s = snapshot();
    let mut state = AppState::default();
    let mut hit_map = HitMap::new();
    hit_map.tabs = vec![(Tab::Images, Rect::new(10, 0, 8, 1))];
    hit_map.rows = vec![(2, Rect::new(0, 5, 40, 1))];
    let click =
        |x, y| InputEvent::Mouse(MouseEvent::new(MouseButton::Left, MouseAction::Press, x, y));
    state.handle(&click(1, 5), &s, &hit_map, 0.0, &[]);
    assert_eq!(state.selected_indexes[&Tab::Containers], 2);
    state.handle(
        &InputEvent::Mouse(MouseEvent::new(
            MouseButton::WheelDown,
            MouseAction::Press,
            1,
            6,
        )),
        &s,
        &hit_map,
        0.0,
        &[],
    );
    assert_eq!(state.selected_indexes[&Tab::Containers], 3);
    state.handle(&click(12, 0), &s, &hit_map, 0.0, &[]);
    assert_eq!(state.tab, Tab::Images);
}

#[test]
fn reconciliation_pins_selection_to_surviving_rows() {
    let mut s = snapshot();
    let mut state = AppState::default();
    press(&mut state, &s, ch('G'));
    s.containers.retain(|c| c.name() != "old");
    state.reconcile_selection(&s);
    assert_eq!(state.selected_indexes[&Tab::Containers], 3);
}

#[test]
fn configuration_seeds_initial_state_and_notices_expire() {
    let mut config = AppConfiguration::default();
    config
        .apply_arguments(&["--running".into(), "--sort".into(), "name".into()])
        .unwrap();
    let mut state = AppState::from_configuration(&config);
    assert_eq!(
        (state.status_filter, state.sort_key, state.sort_descending),
        (StatusFilter::Running, SortKey::Name, false)
    );
    state.post("hi", NoticeLevel::Info, 10.0, Some(2.0));
    state.expire_notice(11.0);
    assert!(state.notice.is_some());
    state.expire_notice(12.0);
    assert!(state.notice.is_none());
}

// MARK: - Multi-host and logs

#[test]
fn brackets_cycle_hosts_and_reset_selection() {
    let s = snapshot();
    let mut state = AppState {
        host_names: vec!["a".into(), "b".into(), "c".into()],
        ..AppState::default()
    };
    press(&mut state, &s, ch('j'));
    assert_eq!(
        press(&mut state, &s, ch('[')),
        vec![AppCommand::SwitchHost(2)]
    );
    assert!(state.selected_ids.is_empty());
    assert_eq!(
        press(&mut state, &s, ch(']')),
        vec![AppCommand::SwitchHost(0)]
    );
    press(&mut state, &s, ch('H'));
    assert_eq!(state.mode, InputMode::HostPicker(0));
    assert_eq!(
        press(&mut state, &s, ch('2')),
        vec![AppCommand::SwitchHost(1)]
    );
}

#[test]
fn host_picker_wraps_and_mouse_selects_the_requested_host() {
    let s = snapshot();
    let mut state = AppState {
        host_names: vec!["local".into(), "prod".into(), "test".into()],
        ..AppState::default()
    };
    press(&mut state, &s, ch('H'));
    press(&mut state, &s, key(Key::Up));
    assert_eq!(state.mode, InputMode::HostPicker(2));
    assert_eq!(
        press(&mut state, &s, key(Key::Enter)),
        vec![AppCommand::SwitchHost(2)]
    );
    assert_eq!(state.active_host, 2);

    let mut hit_map = HitMap::new();
    hit_map.host_chips = vec![(0, Rect::new(10, 0, 8, 1))];
    let click = InputEvent::Mouse(MouseEvent::new(
        MouseButton::Left,
        MouseAction::Press,
        12,
        0,
    ));
    assert_eq!(
        state.handle(&click, &s, &hit_map, 0.0, &[]),
        vec![AppCommand::SwitchHost(0)]
    );
}

#[test]
fn switching_hosts_preserves_filters_but_resets_host_specific_navigation() {
    let mut state = AppState {
        host_names: vec!["local".into(), "remote".into()],
        tab: Tab::Images,
        sort_key: SortKey::Name,
        sort_descending: false,
        filter_text: "alpine".into(),
        detail_scroll: 7,
        log_follow: false,
        ..AppState::default()
    };
    state.selected_ids.insert(Tab::Images, "old-image".into());
    state.scroll_offsets.insert(Tab::Images, 4);
    state.switch_host(1);
    assert_eq!(state.tab, Tab::Images);
    assert_eq!(state.sort_key, SortKey::Name);
    assert!(!state.sort_descending);
    assert_eq!(state.filter_text, "alpine");
    assert!(state.selected_ids.is_empty() && state.scroll_offsets.is_empty());
    assert_eq!(state.detail_scroll, 0);
    assert!(state.log_follow);
}

#[test]
fn single_host_shortcuts_explain_how_to_add_hosts() {
    let s = snapshot();
    let mut state = AppState {
        host_names: vec!["local".into()],
        ..AppState::default()
    };
    for shortcut in ['[', ']', 'H'] {
        assert!(press(&mut state, &s, ch(shortcut)).is_empty());
        assert_eq!(
            state.notice.as_ref().map(|notice| notice.text.as_str()),
            Some(dtop_app::reducer::SINGLE_HOST_HINT)
        );
        assert_eq!(state.mode, InputMode::Normal);
    }
}

#[test]
fn f_follows_and_scrolling_back_pauses() {
    let s = snapshot();
    let mut state = AppState {
        detail_pane: DetailPane::Logs,
        ..AppState::default()
    };
    press(&mut state, &s, ch('f'));
    assert!(!state.log_follow);
    press(&mut state, &s, ch('f'));
    assert!(state.log_follow);
    let mut hit_map = HitMap::new();
    hit_map.detail_rect = Rect::new(50, 0, 30, 20);
    let wheel = |b| InputEvent::Mouse(MouseEvent::new(b, MouseAction::Press, 60, 5));
    state.handle(&wheel(MouseButton::WheelUp), &s, &hit_map, 0.0, &[]);
    assert_eq!((state.detail_scroll, state.log_follow), (3, false));
    state.handle(&wheel(MouseButton::WheelDown), &s, &hit_map, 0.0, &[]);
    assert_eq!((state.detail_scroll, state.log_follow), (0, true));
}

#[test]
fn streamed_logs_are_capped() {
    let mut detail = ContainerDetailData::new("x");
    let lines: Vec<LogLine> = (0..5_010)
        .map(|i| LogLine::new(LogStreamKind::Stdout, &i.to_string()))
        .collect();
    assert_eq!(detail.append_logs(lines), 10);
    assert_eq!(detail.logs[0].text, "10");
    assert_eq!(log_policy::scroll_back(4_990, 20, 5_000), 5_000);
}

#[test]
fn header_shows_host_chips_and_log_status() {
    let s = snapshot();
    let state = AppState {
        host_names: vec!["local".into(), "prod".into()],
        ..AppState::default()
    };
    let hosts = vec![
        HostSummary::new("local", "unix:///x", &s),
        HostSummary::new("prod", "tcp://p:2376 (TLS)", &DockerSnapshot::default()),
    ];
    let frame = render_with(&state, &s, 140, 40, &hosts);
    let header = frame.buffer.text_row(0);
    assert!(
        header.contains("● local") && header.contains("○ prod"),
        "{header}"
    );
    assert_eq!(frame.hit_map.host_chips.len(), 2);
    let mut logs = s;
    logs.detail = Some(ContainerDetailData {
        log_state: LogStreamState::Live,
        ..ContainerDetailData::new("aaa")
    });
    let state = AppState {
        detail_pane: DetailPane::Logs,
        ..AppState::default()
    };
    assert!(render(&state, &logs, 140, 40)
        .buffer
        .plain_text()
        .contains("● live"));
    let paused = AppState {
        is_paused: true,
        ..state
    };
    assert!(render(&paused, &logs, 140, 40)
        .buffer
        .plain_text()
        .contains("⏸ updates paused"));
}

// MARK: - Views

#[test]
fn layout_places_details_beside_or_below() {
    assert!(ScreenLayout::compute(
        TerminalSize {
            columns: 39,
            rows: 30
        },
        true
    )
    .is_none());
    let wide = ScreenLayout::compute(
        TerminalSize {
            columns: 140,
            rows: 40,
        },
        true,
    )
    .unwrap();
    assert_eq!(wide.list.y, wide.detail.unwrap().y);
    let tall = ScreenLayout::compute(
        TerminalSize {
            columns: 100,
            rows: 50,
        },
        true,
    )
    .unwrap();
    assert!(tall.detail.unwrap().y > tall.list.y);
    assert!(ScreenLayout::compute(
        TerminalSize {
            columns: 100,
            rows: 20
        },
        true
    )
    .unwrap()
    .metrics
    .is_none());
}

#[test]
fn renders_header_metrics_tabs_rows_and_details() {
    let s = snapshot();
    let state = AppState::default();
    let frame = render(&state, &s, 140, 40);
    let text = frame.buffer.plain_text();
    let header = frame.buffer.text_row(0);
    assert!(
        header.contains("docker 27.0.0")
            && header.contains("4 cpu")
            && header.contains("12:34:56")
            && header.contains("● 3 up"),
        "{header}"
    );
    assert!(text.contains("cpu") && text.contains("mem") && text.contains("1 Containers 5"));
    assert!(text.contains("web") && text.contains("running"));
    assert!(text.contains(" Overview ") && text.contains("Processes"));
    assert_eq!(frame.hit_map.tabs.len(), 5);
    assert_eq!(frame.hit_map.rows.len(), 5);
}

#[test]
fn empty_states_and_too_small_terminals_explain_themselves() {
    let mut s = snapshot();
    s.containers.clear();
    assert!(render(&AppState::default(), &s, 120, 30)
        .buffer
        .plain_text()
        .contains("No containers yet"));
    s.connection = ConnectionState::Disconnected {
        message: "Cannot connect".into(),
        retry_at: Some(103.0),
    };
    let text = render(&AppState::default(), &s, 120, 30)
        .buffer
        .plain_text();
    assert!(text.contains("Retrying in 3s") && text.contains("✕ offline"));
    assert!(render(&AppState::default(), &snapshot(), 30, 8)
        .buffer
        .plain_text()
        .contains("Terminal too small"));
}

#[test]
fn overlays_render_and_expose_buttons() {
    let s = snapshot();
    let help = AppState {
        mode: InputMode::Help,
        ..AppState::default()
    };
    assert!(render(&help, &s, 120, 40)
        .buffer
        .plain_text()
        .contains("move selection"));
    let operation = DockerOperation::RemoveVolume {
        name: "data".into(),
    };
    let confirm = AppState {
        mode: InputMode::Confirm(Confirmation {
            operation,
            confirm_focused: false,
        }),
        ..AppState::default()
    };
    let frame = render(&confirm, &s, 120, 40);
    assert!(frame.buffer.plain_text().contains("deleted permanently"));
    assert!(frame.hit_map.confirm_button.is_some() && frame.hit_map.cancel_button.is_some());
}

#[test]
fn rendering_never_panics_across_sizes_tabs_themes_and_modes() {
    let s = snapshot();
    for theme in Theme::built_in() {
        for tab in Tab::ALL {
            for (columns, rows) in [(40, 10), (80, 24), (119, 33), (200, 60), (1, 1)] {
                for mode in [InputMode::Normal, InputMode::Help, InputMode::Filter] {
                    let state = AppState {
                        tab,
                        mode,
                        theme_name: theme.name.clone(),
                        ..AppState::default()
                    };
                    render(&state, &s, columns, rows);
                }
            }
        }
    }
    let mut state = AppState::default();
    state.open_theme_editor(Theme::harbor(), Theme::built_in());
    assert!(render(&state, &s, 140, 40)
        .buffer
        .plain_text()
        .contains("theme · harbor"));
}

#[test]
fn identical_frames_render_nothing_the_second_time() {
    let s = snapshot();
    let state = AppState::default();
    let mut renderer = FrameRenderer::new(ColorMode::TrueColor);
    assert!(!renderer
        .render(render(&state, &s, 120, 40).buffer)
        .is_empty());
    assert_eq!(renderer.render(render(&state, &s, 120, 40).buffer), "");
}

#[test]
fn once_report_prints_a_stable_table_and_host_sections() {
    let s = snapshot();
    let text = once::render(&s, 120);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("dtop ") && lines[0].contains("docker 27.0.0"));
    assert!(lines[3].starts_with("NAME"));
    assert!(lines[4].starts_with("web"));
    let multi = once::render_hosts(
        &[
            ("a".into(), s),
            (
                "b".into(),
                DockerSnapshot {
                    connection: ConnectionState::Disconnected {
                        message: "down".into(),
                        retry_at: None,
                    },
                    ..DockerSnapshot::default()
                },
            ),
        ],
        80,
    );
    assert!(multi.contains("== a ==") && multi.contains("== b ==\ndtop: down"));
}

#[test]
fn once_report_neutralizes_escape_sequences_from_the_daemon() {
    let mut s = snapshot();
    s.endpoint = "unix:///x\u{1b}]0;title\u{7}".into();
    s.containers = vec![compose(
        "fff",
        "evil\u{1b}[2J",
        "proj\nfake-row running 99.0",
        "svc",
        ContainerState::Running,
    )];
    let text = once::render(&s, 200);
    assert!(
        !text.chars().any(|c| c.is_control() && c != '\n'),
        "{text:?}"
    );
    // Header, blank line, column titles, one row: no forged line.
    assert_eq!(text.lines().count(), 5, "{text}");
    assert!(text.contains("evil\u{FFFD}[2J"));

    let multi = once::render_hosts(
        &[
            ("a\u{1b}[31m".into(), s),
            (
                "b".into(),
                DockerSnapshot {
                    connection: ConnectionState::Disconnected {
                        message: "down\u{1b}[2J\nforged".into(),
                        retry_at: None,
                    },
                    ..DockerSnapshot::default()
                },
            ),
        ],
        200,
    );
    assert!(
        !multi.chars().any(|c| c.is_control() && c != '\n'),
        "{multi:?}"
    );
    assert!(multi.contains("dtop: down\u{FFFD}[2J\u{FFFD}forged\n"));
}

// MARK: - Refresher

fn refresher(api: Arc<FakeApi>, runner: Option<Arc<FakeRunner>>) -> DataRefresher {
    let mut options = RefresherOptions {
        clock: Arc::new(|| 1_000.0),
        random: Arc::new(|| 0.5),
        backoff: Backoff::new(1.0, 8.0, 2.0, 0.0),
        stats_concurrency: 2,
        ..RefresherOptions::default()
    };
    if let Some(runner) = runner {
        options.command_runner = runner;
    }
    DataRefresher::new(api, "unix:///x", options)
}

#[test]
fn refresh_lists_containers_samples_running_ones_and_records_metrics() {
    let api = Arc::new(FakeApi::default());
    *api.containers.lock().unwrap() = vec![
        container("a", "a", ContainerState::Running),
        container("b", "b", ContainerState::Exited),
    ];
    let r = refresher(Arc::clone(&api), None);
    let s = r.refresh(false);
    assert_eq!(s.connection, ConnectionState::Connected);
    assert_eq!(s.containers.len(), 2);
    assert_eq!(api.stats_calls.load(Ordering::SeqCst), 1);
    assert!(s.metrics.latest.contains_key("a"));
    assert!(s.info.is_some());
}

#[test]
fn inventory_failures_become_warnings() {
    let api = Arc::new(FakeApi {
        fail_volumes: true,
        ..FakeApi::default()
    });
    let s = refresher(api, None).refresh(true);
    assert_eq!(s.connection, ConnectionState::Connected);
    assert_eq!(s.images.len(), 2);
    assert!(s.warnings[0].starts_with("volumes:"));
}

#[test]
fn failures_schedule_retries_with_backoff_and_recover() {
    let api = Arc::new(FakeApi::default());
    *api.fail_list.lock().unwrap() = Some(DockerError::ConnectionFailed {
        endpoint: "x".into(),
        reason: "refused".into(),
    });
    let r = refresher(Arc::clone(&api), None);
    let retry = |s: DockerSnapshot| match s.connection {
        ConnectionState::Disconnected { retry_at, .. } => retry_at,
        _ => None,
    };
    assert_eq!(retry(r.refresh(false)), Some(1_001.0));
    assert_eq!(retry(r.refresh(false)), Some(1_002.0));
    assert_eq!(retry(r.refresh(false)), Some(1_004.0));
    *api.fail_list.lock().unwrap() = None;
    assert_eq!(r.refresh(false).connection, ConnectionState::Connected);
}

#[test]
fn stats_fan_out_respects_the_limit_and_tolerates_failures() {
    let api = Arc::new(FakeApi::default());
    let ids: Vec<String> = (0..10)
        .map(|i| format!("c{i}"))
        .chain(["flaky".to_string()])
        .collect();
    let results = refresher(Arc::clone(&api), None).fetch_stats(&ids).unwrap();
    assert_eq!(results.len(), 10);
    assert!(api.max_in_flight.load(Ordering::SeqCst) <= 2);
}

#[test]
fn failed_stats_workers_are_reported_instead_of_returning_partial_success() {
    let api = Arc::new(FakeApi::default());
    let error = refresher(api, None)
        .fetch_stats(&["panic".into()])
        .unwrap_err();
    assert!(error.to_string().contains("stats worker failed"));
}

#[test]
fn refresh_hundreds_of_containers_keeps_stats_concurrency_bounded() {
    const CONTAINERS: usize = 512;
    let api = Arc::new(FakeApi::default());
    *api.containers.lock().unwrap() = (0..CONTAINERS)
        .map(|index| {
            let name = format!("container-{index}");
            container(&name, &name, ContainerState::Running)
        })
        .collect();
    let options = RefresherOptions {
        history_size: 4,
        stats_concurrency: 8,
        clock: Arc::new(|| 1_000.0),
        ..RefresherOptions::default()
    };
    let refresher = DataRefresher::new(api.clone(), "synthetic", options);
    let started = std::time::Instant::now();
    let snapshot = refresher.refresh(false);
    let elapsed = started.elapsed();
    assert_eq!(snapshot.connection, ConnectionState::Connected);
    assert_eq!(snapshot.containers.len(), CONTAINERS);
    assert_eq!(snapshot.metrics.latest.len(), CONTAINERS);
    assert_eq!(api.stats_calls.load(Ordering::SeqCst), CONTAINERS);
    let peak = api.max_in_flight.load(Ordering::SeqCst);
    assert!((1..=8).contains(&peak));
    eprintln!("synthetic {CONTAINERS}-container refresh: {elapsed:?}; peak stats requests: {peak}");
}

#[test]
fn detail_loading_fills_each_pane_and_captures_errors() {
    let r = refresher(Arc::new(FakeApi::default()), None);
    assert_eq!(
        r.load_detail("a", DetailPane::Overview, true)
            .details
            .unwrap()
            .command_line(),
        "nginx -g"
    );
    assert_eq!(
        r.load_detail("a", DetailPane::Processes, true)
            .processes
            .len(),
        1
    );
    assert_eq!(r.load_detail("a", DetailPane::Logs, true).logs.len(), 2);
    assert!(r
        .load_detail("broken", DetailPane::Overview, true)
        .error
        .unwrap()
        .contains("No such container"));
}

#[test]
fn operations_report_outcomes() {
    let api = Arc::new(FakeApi {
        fail_action_for: Some("bad".into()),
        ..FakeApi::default()
    });
    let r = refresher(Arc::clone(&api), None);
    let op = DockerOperation::Container {
        action: ContainerAction::Stop {
            timeout_seconds: None,
        },
        id: "a".into(),
        name: "web".into(),
    };
    let result = r.perform(&op);
    assert!(result.succeeded && result.message == "Stopped web");
    let prune = r.perform(&DockerOperation::Prune(PruneTarget::Containers));
    assert_eq!(
        prune.message,
        "Pruned containers: 2 removed, 2.0KiB reclaimed"
    );
    let failed = r.perform(&DockerOperation::Container {
        action: ContainerAction::Start,
        id: "bad".into(),
        name: "bad".into(),
    });
    assert!(!failed.succeeded && failed.message.contains("boom"));
}

#[test]
fn compose_operations_use_the_engine_or_the_cli() {
    let api = Arc::new(FakeApi::default());
    *api.containers.lock().unwrap() = vec![
        compose("a", "app-web-1", "app", "web", ContainerState::Running),
        compose("b", "app-db-1", "app", "db", ContainerState::Exited),
    ];
    let runner = Arc::new(FakeRunner(Mutex::new(Vec::new()), 0));
    let r = refresher(Arc::clone(&api), Some(Arc::clone(&runner)));
    let stop = r.perform(&DockerOperation::Compose {
        operation: ComposeOperation::Stop,
        project: "app".into(),
    });
    assert_eq!(stop.message, "compose stop app: 1 container(s)");
    assert_eq!(
        api.actions.lock().unwrap().as_slice(),
        &[("stop".to_string(), "a".to_string())]
    );
    let up = r.perform(&DockerOperation::Compose {
        operation: ComposeOperation::Up,
        project: "app".into(),
    });
    assert!(up.succeeded);
    let invocation = runner.0.lock().unwrap()[0].clone();
    assert_eq!(invocation.display_string(), "docker compose --project-name app --project-directory /srv/app --file /srv/app/compose.yaml up --detach");
    let missing = r.perform(&DockerOperation::Compose {
        operation: ComposeOperation::Up,
        project: "gone".into(),
    });
    assert!(missing.message.contains("has no containers"));
    let failing = refresher(api, Some(Arc::new(FakeRunner(Mutex::new(Vec::new()), 1))));
    assert!(failing
        .perform(&DockerOperation::Compose {
            operation: ComposeOperation::Down,
            project: "app".into()
        })
        .message
        .ends_with("failed: last line"));
}

#[test]
fn ctrl_modifier_keys_refresh() {
    let s = snapshot();
    let mut state = AppState::default();
    let event = InputEvent::Key(KeyEvent::with(Key::Char('l'), Modifiers::CTRL));
    assert_eq!(
        press(&mut state, &s, event),
        vec![AppCommand::Refresh { inventory: false }]
    );
    assert_eq!(
        press(&mut state, &s, key(Key::F(5))),
        vec![AppCommand::Refresh { inventory: true }]
    );
}
