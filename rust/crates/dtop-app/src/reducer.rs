//! The reducer: one input event in, state changes plus side effects (`AppCommand`) out.

use dtop_core::compose::{ComposeOperation, ComposeStatus};
use dtop_core::models::ContainerState;
use dtop_core::ContainerAction;
use dtop_ui::buffer::Rect;
use dtop_ui::{InputEvent, Key, KeyEvent, Modifiers, MouseAction, MouseButton, MouseEvent, Theme};

use crate::list::{self, ListItem};
use crate::operation::{DockerOperation, PruneTarget};
use crate::state::*;
use crate::theme_editor::Outcome;

/// Side effects the runtime executes.
#[derive(Debug, Clone, PartialEq)]
pub enum AppCommand {
    Quit,
    Refresh {
        inventory: bool,
    },
    Perform(DockerOperation),
    ThemeChanged(String),
    /// Open the theme menu; the runtime supplies the current and available themes.
    OpenThemeEditor,
    SaveTheme {
        name: String,
        theme: Box<Theme>,
    },
    /// Show another host (index into `AppState::host_names`).
    SwitchHost(usize),
}

/// Screen regions from the last frame, for routing mouse events.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HitMap {
    pub tabs: Vec<(Tab, Rect)>,
    pub rows: Vec<(usize, Rect)>,
    pub list_rect: Rect,
    pub list_visible_rows: usize,
    pub list_scroll_offset: usize,
    pub detail_rect: Rect,
    pub detail_panes: Vec<(DetailPane, Rect)>,
    pub editor_rows: Vec<(usize, Rect)>,
    pub confirm_button: Option<Rect>,
    pub cancel_button: Option<Rect>,
    /// Host chips in the header and rows of the host list.
    pub host_chips: Vec<(usize, Rect)>,
    pub host_rows: Vec<(usize, Rect)>,
}

impl HitMap {
    pub fn new() -> HitMap {
        HitMap {
            list_visible_rows: 10,
            ..Default::default()
        }
    }
}

pub const SINGLE_HOST_HINT: &str =
    "Only one Docker host — add more with --host, --context or --all-contexts";

fn hit<T: Copy>(hits: &[(T, Rect)], x: i32, y: i32) -> Option<T> {
    hits.iter().find(|(_, r)| r.contains(x, y)).map(|(v, _)| *v)
}

impl AppState {
    /// Applies one input event and returns the side effects to run.
    pub fn handle(
        &mut self,
        event: &InputEvent,
        snapshot: &DockerSnapshot,
        hit_map: &HitMap,
        now: f64,
        theme_names: &[String],
    ) -> Vec<AppCommand> {
        if let InputEvent::Key(key) = event {
            if key.key == Key::Char('c') && key.modifiers.contains(Modifiers::CTRL) {
                self.should_quit = true;
                return vec![AppCommand::Quit];
            }
        }
        match self.mode.clone() {
            InputMode::Help => {
                match event {
                    InputEvent::Key(_) => self.mode = InputMode::Normal,
                    InputEvent::Mouse(m) if m.action == MouseAction::Press && !m.is_wheel() => {
                        self.mode = InputMode::Normal
                    }
                    _ => {}
                }
                Vec::new()
            }
            InputMode::Filter => self.handle_filter(event),
            InputMode::HostPicker(selected) => {
                self.handle_host_picker(event, selected, hit_map, now)
            }
            InputMode::Confirm(confirmation) => self.handle_confirm(event, confirmation, hit_map),
            InputMode::ThemeEditor(mut editor) => {
                match editor.handle(event, &hit_map.editor_rows) {
                    Outcome::Editing => self.mode = InputMode::ThemeEditor(editor),
                    Outcome::Cancelled => {
                        self.mode = InputMode::Normal;
                        if editor.is_dirty() {
                            self.post("Theme changes discarded", NoticeLevel::Info, now, Some(3.0));
                        }
                    }
                    Outcome::Save { name, theme } => {
                        self.mode = InputMode::ThemeEditor(editor);
                        return vec![AppCommand::SaveTheme {
                            name,
                            theme: Box::new(theme),
                        }];
                    }
                }
                Vec::new()
            }
            InputMode::Normal => match event {
                InputEvent::Key(key) => self.handle_key(key, snapshot, hit_map, now, theme_names),
                InputEvent::Mouse(mouse) => self.handle_mouse(mouse, snapshot, hit_map),
                InputEvent::Paste(text) => {
                    self.mode = InputMode::Filter;
                    self.handle_filter(&InputEvent::Paste(text.clone()))
                }
                InputEvent::Focus(_) => Vec::new(),
            },
        }
    }

    fn handle_key(
        &mut self,
        event: &KeyEvent,
        snapshot: &DockerSnapshot,
        hit_map: &HitMap,
        now: f64,
        theme_names: &[String],
    ) -> Vec<AppCommand> {
        let items = list::items(self.tab, snapshot, self);
        let page = hit_map.list_visible_rows.saturating_sub(1).max(1) as i64;
        let ctrl = event.modifiers.contains(Modifiers::CTRL);
        match event.key {
            Key::Up => self.move_selection_by(-1, &items),
            Key::Down => self.move_selection_by(1, &items),
            Key::PageUp => self.move_selection_by(-page, &items),
            Key::PageDown => self.move_selection_by(page, &items),
            Key::Home => self.move_selection_to(0, &items),
            Key::End => self.move_selection_to(items.len() as i64 - 1, &items),
            Key::Tab | Key::Right => self.switch_tab(self.tab.next()),
            Key::BackTab | Key::Left => self.switch_tab(self.tab.previous()),
            Key::F(1) => self.mode = InputMode::Help,
            Key::F(2) => return vec![AppCommand::OpenThemeEditor],
            Key::F(5) => return vec![AppCommand::Refresh { inventory: true }],
            Key::Escape => {
                if !self.filter_text.is_empty() {
                    self.set_filter(String::new());
                } else {
                    self.notice = None;
                }
            }
            Key::Enter => {
                if let Some(ListItem::ComposeProject { project, .. }) = self.selected(&items) {
                    self.toggle_collapsed(&project.name);
                } else {
                    self.show_details = !self.show_details;
                }
            }
            Key::Char(c) if ctrl => {
                if c == 'r' {
                    return vec![AppCommand::Refresh { inventory: true }];
                }
                if c == 'l' {
                    return vec![AppCommand::Refresh { inventory: false }];
                }
            }
            Key::Char(c) => return self.handle_character(c, &items, now, theme_names),
            _ => {}
        }
        Vec::new()
    }

    fn handle_character(
        &mut self,
        c: char,
        items: &[ListItem],
        now: f64,
        theme_names: &[String],
    ) -> Vec<AppCommand> {
        match c {
            'q' | 'Q' => {
                self.should_quit = true;
                return vec![AppCommand::Quit];
            }
            '?' => self.mode = InputMode::Help,
            '/' => self.mode = InputMode::Filter,
            'k' => self.move_selection_by(-1, items),
            'j' => self.move_selection_by(1, items),
            'g' => self.move_selection_to(0, items),
            'G' => self.move_selection_to(items.len() as i64 - 1, items),
            'l' => self.switch_tab(self.tab.next()),
            'h' => self.switch_tab(self.tab.previous()),
            '1'..='5' => {
                let index = (c as u8 - b'1') as usize;
                self.switch_tab(Tab::ALL[index]);
            }
            's' => {
                self.sort_key = self.sort_key.next();
                self.sort_descending = self.sort_key.prefers_descending();
                self.post_sort(now);
            }
            'S' => {
                self.sort_descending = !self.sort_descending;
                self.post_sort(now);
            }
            'a' => {
                self.status_filter = self.status_filter.next();
                self.reset_selection();
                let text = format!("Showing: {} containers", self.status_filter.raw_value());
                self.post(text, NoticeLevel::Info, now, Some(2.0));
            }
            'd' => self.show_details = !self.show_details,
            'v' => {
                self.detail_pane = self.detail_pane.next();
                self.detail_scroll = 0;
                self.log_follow = true;
                self.show_details = true;
            }
            'f' => {
                if self.detail_pane != DetailPane::Logs || !self.show_details {
                    self.post(
                        "f follows the log stream — open the Logs pane with v",
                        NoticeLevel::Info,
                        now,
                        Some(3.0),
                    );
                } else if self.log_follow {
                    self.log_follow = false;
                    self.post(
                        "Log view paused — f to follow again",
                        NoticeLevel::Info,
                        now,
                        Some(2.0),
                    );
                } else {
                    self.log_follow = true;
                    self.detail_scroll = 0;
                }
            }
            'c' => {
                if let Some(ListItem::ComposeProject { project, .. }) = self.selected(items) {
                    self.toggle_collapsed(&project.name);
                }
            }
            't' => {
                if theme_names.is_empty() {
                    return Vec::new();
                }
                let index = theme_names
                    .iter()
                    .position(|n| *n == self.theme_name)
                    .map_or(0, |i| (i + 1) % theme_names.len());
                self.theme_name = theme_names[index].clone();
                let text = format!("Theme: {}", self.theme_name);
                self.post(text, NoticeLevel::Info, now, Some(2.0));
                return vec![AppCommand::ThemeChanged(self.theme_name.clone())];
            }
            'T' => return vec![AppCommand::OpenThemeEditor],
            ' ' => {
                self.is_paused = !self.is_paused;
                // Resuming while following the log stream jumps back to the newest line.
                if !self.is_paused && self.log_follow && self.detail_pane == DetailPane::Logs {
                    self.detail_scroll = 0;
                }
                if self.is_paused {
                    self.post(
                        "Updates paused — press space to resume",
                        NoticeLevel::Info,
                        now,
                        None,
                    );
                } else {
                    self.post("Updates resumed", NoticeLevel::Info, now, Some(2.0));
                }
            }
            'R' => return vec![AppCommand::Refresh { inventory: true }],
            ']' | '[' => {
                if !self.has_several_hosts() {
                    self.post(SINGLE_HOST_HINT, NoticeLevel::Info, now, Some(4.0));
                    return Vec::new();
                }
                let count = self.host_names.len();
                let step = if c == ']' { 1 } else { count - 1 };
                let target = (self.active_host + step) % count;
                self.switch_host(target);
                let text = format!("Host: {}", self.host_names[target]);
                self.post(text, NoticeLevel::Info, now, Some(2.0));
                return vec![AppCommand::SwitchHost(target)];
            }
            'H' => {
                if !self.has_several_hosts() {
                    self.post(SINGLE_HOST_HINT, NoticeLevel::Info, now, Some(4.0));
                } else {
                    self.mode = InputMode::HostPicker(self.active_host);
                }
            }
            'u' | 'x' | 'r' | 'p' | 'K' | 'D' | 'U' | 'X' => {
                let item = self.selected(items);
                return self.request_operation(c, item.as_ref(), now);
            }
            _ => {}
        }
        Vec::new()
    }

    fn post_sort(&mut self, now: f64) {
        let text = format!(
            "Sort: {} {}",
            self.sort_key.raw_value(),
            if self.sort_descending { "↓" } else { "↑" }
        );
        self.post(text, NoticeLevel::Info, now, Some(2.0));
    }

    fn handle_host_picker(
        &mut self,
        event: &InputEvent,
        selected: usize,
        hit_map: &HitMap,
        now: f64,
    ) -> Vec<AppCommand> {
        let count = self.host_names.len();
        if count == 0 {
            self.mode = InputMode::Normal;
            return Vec::new();
        }
        let choose = |state: &mut AppState, index: usize| -> Vec<AppCommand> {
            state.mode = InputMode::Normal;
            if index == state.active_host {
                return Vec::new();
            }
            state.switch_host(index);
            let text = format!("Host: {}", state.host_names[index]);
            state.post(text, NoticeLevel::Info, now, Some(2.0));
            vec![AppCommand::SwitchHost(index)]
        };
        match event {
            InputEvent::Key(key) => match key.key {
                Key::Up | Key::Char('k') => {
                    self.mode = InputMode::HostPicker((selected + count - 1) % count)
                }
                Key::Down | Key::Char('j') | Key::Tab => {
                    self.mode = InputMode::HostPicker((selected + 1) % count)
                }
                Key::Home => self.mode = InputMode::HostPicker(0),
                Key::End => self.mode = InputMode::HostPicker(count - 1),
                Key::Enter | Key::Char(' ') => return choose(self, selected),
                Key::Escape | Key::Char('q' | 'H') => self.mode = InputMode::Normal,
                Key::Char(c) => {
                    if let Some(digit) = c
                        .to_digit(10)
                        .map(|d| d as usize)
                        .filter(|d| (1..=count).contains(d))
                    {
                        return choose(self, digit - 1);
                    }
                }
                _ => {}
            },
            InputEvent::Mouse(mouse) if mouse.action == MouseAction::Press => {
                if mouse.is_wheel() {
                    let delta = if mouse.button == MouseButton::WheelUp {
                        count - 1
                    } else {
                        1
                    };
                    self.mode = InputMode::HostPicker((selected + delta) % count);
                } else if let Some(index) = hit(&hit_map.host_rows, mouse.x, mouse.y) {
                    return choose(self, index);
                } else {
                    self.mode = InputMode::Normal;
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn request_operation(
        &mut self,
        key: char,
        item: Option<&ListItem>,
        now: f64,
    ) -> Vec<AppCommand> {
        let operation = if key == 'X' {
            DockerOperation::Prune(match self.tab {
                Tab::Containers | Tab::Compose => PruneTarget::Containers,
                Tab::Images => PruneTarget::Images,
                Tab::Networks => PruneTarget::Networks,
                Tab::Volumes => PruneTarget::Volumes,
            })
        } else {
            let Some(item) = item else {
                self.post("Nothing selected", NoticeLevel::Warning, now, Some(4.0));
                return Vec::new();
            };
            match operation_for(key, item) {
                Some(op) => op,
                None => {
                    self.post(
                        unavailable_message(key, item),
                        NoticeLevel::Warning,
                        now,
                        Some(4.0),
                    );
                    return Vec::new();
                }
            }
        };
        if operation.is_destructive() {
            self.mode = InputMode::Confirm(Confirmation {
                operation,
                confirm_focused: false,
            });
            return Vec::new();
        }
        self.post(
            operation.progress_description(),
            NoticeLevel::Info,
            now,
            None,
        );
        vec![AppCommand::Perform(operation)]
    }

    fn handle_mouse(
        &mut self,
        mouse: &MouseEvent,
        snapshot: &DockerSnapshot,
        hit_map: &HitMap,
    ) -> Vec<AppCommand> {
        let items = list::items(self.tab, snapshot, self);
        match mouse.button {
            MouseButton::WheelUp | MouseButton::WheelDown => {
                let delta: i64 = if mouse.button == MouseButton::WheelUp {
                    -1
                } else {
                    1
                };
                if hit_map.detail_rect.contains(mouse.x, mouse.y) {
                    // The logs pane is anchored to the newest line and scrolls back in time, so
                    // the wheel moves the other way there (wheel up = older lines).
                    let shows_logs = self.detail_pane == DetailPane::Logs
                        && self
                            .selected(&items)
                            .is_some_and(|i| i.container().is_some());
                    let step = if shows_logs { -delta } else { delta } * 3;
                    self.detail_scroll = (self.detail_scroll as i64 + step).max(0) as usize;
                    // Scrolling back pauses following; scrolling all the way down resumes it.
                    if shows_logs {
                        self.log_follow = self.detail_scroll == 0;
                    }
                } else {
                    self.move_selection_by(delta, &items);
                }
            }
            MouseButton::Left if mouse.action == MouseAction::Press => {
                if let Some(index) = hit(&hit_map.host_chips, mouse.x, mouse.y) {
                    if index != self.active_host {
                        self.switch_host(index);
                        return vec![AppCommand::SwitchHost(index)];
                    }
                } else if let Some(tab) = hit(&hit_map.tabs, mouse.x, mouse.y) {
                    self.switch_tab(tab);
                } else if let Some(pane) = hit(&hit_map.detail_panes, mouse.x, mouse.y) {
                    self.detail_pane = pane;
                    self.detail_scroll = 0;
                    self.log_follow = true;
                } else if let Some(index) =
                    hit(&hit_map.rows, mouse.x, mouse.y).filter(|i| *i < items.len())
                {
                    let already = list::selected_index(&items, self, self.tab) == Some(index);
                    self.move_selection_to(index as i64, &items);
                    if already {
                        if let ListItem::ComposeProject { project, .. } = &items[index] {
                            self.toggle_collapsed(&project.name);
                        }
                    }
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn handle_filter(&mut self, event: &InputEvent) -> Vec<AppCommand> {
        match event {
            InputEvent::Key(key) => match key.key {
                Key::Escape => {
                    self.set_filter(String::new());
                    self.mode = InputMode::Normal;
                }
                Key::Enter | Key::Tab | Key::Down | Key::Up => self.mode = InputMode::Normal,
                Key::Backspace => {
                    if !self.filter_text.is_empty() {
                        let mut text = self.filter_text.clone();
                        text.pop();
                        self.set_filter(text);
                    }
                }
                Key::Char(c) if key.modifiers.contains(Modifiers::CTRL) => {
                    if c == 'u' {
                        self.set_filter(String::new());
                    }
                    if c == 'w' {
                        let text = deleting_last_word(&self.filter_text);
                        self.set_filter(text);
                    }
                }
                Key::Char(c) if !key.modifiers.contains(Modifiers::ALT) => {
                    let text = format!("{}{c}", self.filter_text);
                    self.set_filter(text);
                }
                _ => {}
            },
            InputEvent::Paste(text) => {
                let cleaned: String = text
                    .chars()
                    .filter(|c| !matches!(c, '\n' | '\r' | '\t'))
                    .collect();
                let combined = format!("{}{cleaned}", self.filter_text);
                self.set_filter(combined);
            }
            InputEvent::Mouse(mouse) if mouse.action == MouseAction::Press && !mouse.is_wheel() => {
                self.mode = InputMode::Normal
            }
            _ => {}
        }
        Vec::new()
    }

    fn handle_confirm(
        &mut self,
        event: &InputEvent,
        mut confirmation: Confirmation,
        hit_map: &HitMap,
    ) -> Vec<AppCommand> {
        match event {
            InputEvent::Key(key) => match key.key {
                Key::Char('y' | 'Y') => return self.confirm(confirmation.operation),
                Key::Char('n' | 'N' | 'q') | Key::Escape => self.mode = InputMode::Normal,
                Key::Left | Key::Right | Key::Tab | Key::BackTab | Key::Char('h' | 'l') => {
                    confirmation.confirm_focused = !confirmation.confirm_focused;
                    self.mode = InputMode::Confirm(confirmation);
                }
                Key::Enter => {
                    if confirmation.confirm_focused {
                        return self.confirm(confirmation.operation);
                    }
                    self.mode = InputMode::Normal;
                }
                _ => {}
            },
            InputEvent::Mouse(mouse)
                if mouse.action == MouseAction::Press && mouse.button == MouseButton::Left =>
            {
                if hit_map
                    .confirm_button
                    .is_some_and(|r| r.contains(mouse.x, mouse.y))
                {
                    return self.confirm(confirmation.operation);
                }
                if hit_map
                    .cancel_button
                    .is_some_and(|r| r.contains(mouse.x, mouse.y))
                {
                    self.mode = InputMode::Normal;
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn confirm(&mut self, operation: DockerOperation) -> Vec<AppCommand> {
        self.mode = InputMode::Normal;
        self.notice = Some(Notice {
            text: operation.progress_description(),
            level: NoticeLevel::Info,
            expires_at: None,
        });
        vec![AppCommand::Perform(operation)]
    }

    pub fn selected(&self, items: &[ListItem]) -> Option<ListItem> {
        list::selected_index(items, self, self.tab).map(|i| items[i].clone())
    }

    pub fn move_selection_by(&mut self, delta: i64, items: &[ListItem]) {
        if items.is_empty() {
            return;
        }
        let current = list::selected_index(items, self, self.tab).unwrap_or(0) as i64;
        self.move_selection_to(current + delta, items);
    }

    pub fn move_selection_to(&mut self, index: i64, items: &[ListItem]) {
        if items.is_empty() {
            return;
        }
        let clamped = index.clamp(0, items.len() as i64 - 1) as usize;
        let id = items[clamped].id();
        if self.selected_ids.get(&self.tab) != Some(&id) {
            self.detail_scroll = 0;
        }
        self.selected_ids.insert(self.tab, id);
        self.selected_indexes.insert(self.tab, clamped);
    }

    pub fn switch_tab(&mut self, target: Tab) {
        if target != self.tab {
            self.tab = target;
            self.detail_scroll = 0;
        }
    }

    pub fn toggle_collapsed(&mut self, project: &str) {
        if !self.collapsed_projects.remove(project) {
            self.collapsed_projects.insert(project.to_string());
        }
    }

    pub fn set_filter(&mut self, text: String) {
        self.filter_text = text;
        self.reset_selection();
    }

    pub fn reset_selection(&mut self) {
        self.selected_ids.remove(&self.tab);
        self.selected_indexes.insert(self.tab, 0);
        self.scroll_offsets.insert(self.tab, 0);
        self.detail_scroll = 0;
    }

    /// After a refresh, pins each tab's selection to a row that still exists.
    pub fn reconcile_selection(&mut self, snapshot: &DockerSnapshot) {
        for tab in Tab::ALL {
            let rows = list::items(tab, snapshot, self);
            match list::selected_index(&rows, self, tab) {
                Some(index) => {
                    self.selected_ids.insert(tab, rows[index].id());
                    self.selected_indexes.insert(tab, index);
                }
                None => {
                    self.selected_ids.remove(&tab);
                }
            }
        }
    }
}

pub fn deleting_last_word(text: &str) -> String {
    let trimmed = text.trim_end_matches(' ');
    match trimmed.rfind(' ') {
        Some(index) => trimmed[..=index].to_string(),
        None => String::new(),
    }
}

/// The operation a key means for the selected item, or None when it does not apply.
pub fn operation_for(key: char, item: &ListItem) -> Option<DockerOperation> {
    match item {
        ListItem::Container { container, .. } => {
            let (id, name) = (container.id.clone(), container.name());
            let op = |action: ContainerAction| {
                Some(DockerOperation::Container {
                    action,
                    id: id.clone(),
                    name: name.clone(),
                })
            };
            let active = container.state.is_active();
            match key {
                'u' if container.state == ContainerState::Paused => op(ContainerAction::Unpause),
                'u' => (!active).then(|| op(ContainerAction::Start)).flatten(),
                'x' => active
                    .then(|| {
                        op(ContainerAction::Stop {
                            timeout_seconds: None,
                        })
                    })
                    .flatten(),
                'r' => op(ContainerAction::Restart {
                    timeout_seconds: None,
                }),
                'p' if container.state == ContainerState::Running => op(ContainerAction::Pause),
                'p' if container.state == ContainerState::Paused => op(ContainerAction::Unpause),
                'K' => active
                    .then(|| op(ContainerAction::Kill { signal: None }))
                    .flatten(),
                'D' => Some(DockerOperation::RemoveContainer {
                    id: id.clone(),
                    name: name.clone(),
                    force: active,
                }),
                _ => None,
            }
        }
        ListItem::ComposeProject { project, .. } => {
            let op = |operation| {
                Some(DockerOperation::Compose {
                    operation,
                    project: project.name.clone(),
                })
            };
            match key {
                'u' => op(ComposeOperation::Start),
                'U' => op(ComposeOperation::Up),
                'x' => op(ComposeOperation::Stop),
                'r' => op(ComposeOperation::Restart),
                'p' => op(if project.aggregate_status() == ComposeStatus::Paused {
                    ComposeOperation::Unpause
                } else {
                    ComposeOperation::Pause
                }),
                'D' => op(ComposeOperation::Down),
                _ => None,
            }
        }
        ListItem::Image(image) => (key == 'D').then(|| DockerOperation::RemoveImage {
            id: image.id.clone(),
            name: image.display_name(),
            force: false,
        }),
        ListItem::Network(network) => {
            (key == 'D' && !network.is_built_in()).then(|| DockerOperation::RemoveNetwork {
                id: network.id.clone(),
                name: network.name.clone(),
            })
        }
        ListItem::Volume(volume) => (key == 'D').then(|| DockerOperation::RemoveVolume {
            name: volume.name.clone(),
        }),
    }
}

pub fn unavailable_message(key: char, item: &ListItem) -> String {
    match item {
        ListItem::Container { container, .. } => {
            let name = container.name();
            match key {
                'u' => format!("{name} is already running"),
                'x' | 'K' => format!("{name} is not running"),
                'p' => format!("{name} can only be paused while running"),
                _ => "That action does not apply to containers".into(),
            }
        }
        ListItem::Network(network) if network.is_built_in() && key == 'D' => {
            format!(
                "{} is a built-in network and cannot be removed",
                network.name
            )
        }
        _ => "That action does not apply here".into(),
    }
}
