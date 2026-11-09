//! Filtered, sorted rows for each tab. Pure, so it is tested without a terminal.

use std::cmp::Ordering;

use dtop_core::compose::ComposeProject;
use dtop_core::models::{Container, DockerImage, DockerNetwork, DockerVolume};

use crate::state::{AppState, DockerSnapshot, SortKey, Tab};

#[derive(Debug, Clone, PartialEq)]
pub enum ListItem {
    Container {
        container: Container,
        depth: usize,
        is_last: bool,
    },
    ComposeProject {
        project: ComposeProject,
        collapsed: bool,
    },
    Image(DockerImage),
    Network(DockerNetwork),
    Volume(DockerVolume),
}

impl ListItem {
    pub fn id(&self) -> String {
        match self {
            ListItem::Container { container, .. } => container.id.clone(),
            ListItem::ComposeProject { project, .. } => format!("project:{}", project.name),
            ListItem::Image(image) => image.id.clone(),
            ListItem::Network(network) => network.id.clone(),
            ListItem::Volume(volume) => format!("volume:{}", volume.name),
        }
    }

    pub fn container(&self) -> Option<&Container> {
        match self {
            ListItem::Container { container, .. } => Some(container),
            _ => None,
        }
    }

    pub fn compose_project(&self) -> Option<&ComposeProject> {
        match self {
            ListItem::ComposeProject { project, .. } => Some(project),
            _ => None,
        }
    }
}

fn matches(container: &Container, query: &str) -> bool {
    container.name().to_lowercase().contains(query)
        || container.image.to_lowercase().contains(query)
        || container.id.starts_with(query)
        || container
            .compose_project()
            .is_some_and(|p| p.to_lowercase().contains(query))
        || container
            .compose_service()
            .is_some_and(|s| s.to_lowercase().contains(query))
        || container.state.raw_value() == query
}

pub fn items(tab: Tab, snapshot: &DockerSnapshot, state: &AppState) -> Vec<ListItem> {
    let query = state.filter_text.trim().to_lowercase();
    match tab {
        Tab::Containers => {
            let containers: Vec<Container> = snapshot
                .containers
                .iter()
                .filter(|c| state.status_filter.matches(&c.state))
                .filter(|c| query.is_empty() || matches(c, &query))
                .cloned()
                .collect();
            sort_containers(containers, snapshot, state.sort_key, state.sort_descending)
                .into_iter()
                .map(|container| ListItem::Container {
                    container,
                    depth: 0,
                    is_last: false,
                })
                .collect()
        }
        Tab::Compose => {
            let filtered: Vec<Container> = snapshot
                .containers
                .iter()
                .filter(|c| state.status_filter.matches(&c.state))
                .cloned()
                .collect();
            let mut out = Vec::new();
            for project in ComposeProject::group(&filtered).0 {
                let project_matches =
                    query.is_empty() || project.name.to_lowercase().contains(&query);
                let mut members: Vec<Container> = project
                    .containers()
                    .filter(|c| project_matches || matches(c, &query))
                    .cloned()
                    .collect();
                if members.is_empty() {
                    continue;
                }
                let collapsed = state.collapsed_projects.contains(&project.name);
                let name = project.name.clone();
                out.push(ListItem::ComposeProject { project, collapsed });
                if collapsed {
                    continue;
                }
                members.sort_by(|a, b| {
                    let key = |c: &Container| {
                        (
                            c.compose_service().unwrap_or("").to_string(),
                            c.compose_container_number().unwrap_or(0),
                            c.name(),
                        )
                    };
                    key(a).cmp(&key(b))
                });
                let count = members.len();
                let _ = name;
                out.extend(members.into_iter().enumerate().map(|(i, container)| {
                    ListItem::Container {
                        container,
                        depth: 1,
                        is_last: i == count - 1,
                    }
                }));
            }
            out
        }
        Tab::Images => {
            let mut images: Vec<DockerImage> = snapshot
                .images
                .iter()
                .filter(|i| {
                    query.is_empty()
                        || i.display_name().to_lowercase().contains(&query)
                        || i.repo_tags
                            .iter()
                            .any(|t| t.to_lowercase().contains(&query))
                        || i.short_id().starts_with(&query)
                })
                .cloned()
                .collect();
            images.sort_by(|a, b| match state.sort_key {
                SortKey::Created => directed(a.created.cmp(&b.created), state.sort_descending),
                SortKey::Memory => directed(a.size.cmp(&b.size), state.sort_descending),
                _ => a
                    .display_name()
                    .cmp(&b.display_name())
                    .then_with(|| a.id.cmp(&b.id)),
            });
            images.into_iter().map(ListItem::Image).collect()
        }
        Tab::Networks => {
            let mut networks: Vec<DockerNetwork> = snapshot
                .networks
                .iter()
                .filter(|n| {
                    query.is_empty()
                        || n.name.to_lowercase().contains(&query)
                        || n.driver.to_lowercase().contains(&query)
                })
                .cloned()
                .collect();
            networks.sort_by(|a, b| a.name.cmp(&b.name));
            networks.into_iter().map(ListItem::Network).collect()
        }
        Tab::Volumes => {
            let mut volumes: Vec<DockerVolume> = snapshot
                .volumes
                .iter()
                .filter(|v| {
                    query.is_empty()
                        || v.name.to_lowercase().contains(&query)
                        || v.driver.to_lowercase().contains(&query)
                        || v.compose_project()
                            .is_some_and(|p| p.to_lowercase().contains(&query))
                })
                .cloned()
                .collect();
            // Named volumes first: anonymous hashes are rarely what people look for.
            volumes.sort_by(|a, b| {
                a.is_anonymous()
                    .cmp(&b.is_anonymous())
                    .then_with(|| a.name.cmp(&b.name))
            });
            volumes.into_iter().map(ListItem::Volume).collect()
        }
    }
}

fn directed(ordering: Ordering, descending: bool) -> Ordering {
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
}

fn partial(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

pub fn sort_containers(
    mut containers: Vec<Container>,
    snapshot: &DockerSnapshot,
    key: SortKey,
    descending: bool,
) -> Vec<Container> {
    let latest = &snapshot.metrics.latest;
    let compare = |a: &Container, b: &Container| -> Ordering {
        let la = latest.get(&a.id);
        let lb = latest.get(&b.id);
        match key {
            SortKey::Name => a.name().cmp(&b.name()),
            SortKey::Cpu => partial(
                la.map_or(-1.0, |s| s.cpu_percent),
                lb.map_or(-1.0, |s| s.cpu_percent),
            ),
            SortKey::Memory => la
                .map_or(0, |s| s.memory_usage)
                .cmp(&lb.map_or(0, |s| s.memory_usage)),
            SortKey::Gpu => partial(
                la.and_then(|s| s.gpu_percent).unwrap_or(-1.0),
                lb.and_then(|s| s.gpu_percent).unwrap_or(-1.0),
            )
            .then_with(|| {
                la.and_then(|s| s.gpu_memory)
                    .unwrap_or(0)
                    .cmp(&lb.and_then(|s| s.gpu_memory).unwrap_or(0))
            }),
            SortKey::Status => a.state.sort_rank().cmp(&b.state.sort_rank()),
            SortKey::Created => a.created.cmp(&b.created),
            SortKey::Image => a.image.cmp(&b.image),
        }
    };
    containers.sort_by(|a, b| {
        let ordered = compare(a, b);
        if ordered != Ordering::Equal {
            return directed(ordered, descending);
        }
        // Stable, deterministic tie-breakers: running first, then name, then ID.
        a.state
            .sort_rank()
            .cmp(&b.state.sort_rank())
            .then_with(|| a.name().cmp(&b.name()))
            .then_with(|| a.id.cmp(&b.id))
    });
    containers
}

/// The selected row from the remembered ID, else the remembered index.
pub fn selected_index(items: &[ListItem], state: &AppState, tab: Tab) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    if let Some(id) = state.selected_ids.get(&tab) {
        if let Some(index) = items.iter().position(|i| &i.id() == id) {
            return Some(index);
        }
    }
    Some(
        state
            .selected_indexes
            .get(&tab)
            .copied()
            .unwrap_or(0)
            .min(items.len() - 1),
    )
}

pub fn selected_item(snapshot: &DockerSnapshot, state: &AppState) -> Option<ListItem> {
    let rows = items(state.tab, snapshot, state);
    selected_index(&rows, state, state.tab).map(|i| rows[i].clone())
}
