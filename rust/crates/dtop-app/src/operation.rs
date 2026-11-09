//! Management actions. Destructive ones go through a confirmation dialog.

use dtop_core::compose::ComposeOperation;
use dtop_core::ContainerAction;

use crate::state::{capitalized_first, past_verb, progress_verb};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneTarget {
    Containers,
    Images,
    Networks,
    Volumes,
}

impl PruneTarget {
    pub fn raw_value(self) -> &'static str {
        match self {
            PruneTarget::Containers => "containers",
            PruneTarget::Images => "images",
            PruneTarget::Networks => "networks",
            PruneTarget::Volumes => "volumes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerOperation {
    Container {
        action: ContainerAction,
        id: String,
        name: String,
    },
    RemoveContainer {
        id: String,
        name: String,
        force: bool,
    },
    Compose {
        operation: ComposeOperation,
        project: String,
    },
    RemoveImage {
        id: String,
        name: String,
        force: bool,
    },
    RemoveNetwork {
        id: String,
        name: String,
    },
    RemoveVolume {
        name: String,
    },
    Prune(PruneTarget),
}

impl DockerOperation {
    pub fn is_destructive(&self) -> bool {
        match self {
            DockerOperation::Container { action, .. } => {
                matches!(action, ContainerAction::Kill { .. })
            }
            DockerOperation::Compose { operation, .. } => operation.is_destructive(),
            _ => true,
        }
    }

    pub fn confirmation_title(&self) -> String {
        match self {
            DockerOperation::Container { action, .. } => {
                format!("{} container?", capitalized_first(action.verb()))
            }
            DockerOperation::RemoveContainer { .. } => "Remove container?".into(),
            DockerOperation::Compose { operation, .. } => {
                format!("Compose {}?", operation.raw_value())
            }
            DockerOperation::RemoveImage { .. } => "Remove image?".into(),
            DockerOperation::RemoveNetwork { .. } => "Remove network?".into(),
            DockerOperation::RemoveVolume { .. } => "Remove volume?".into(),
            DockerOperation::Prune(target) => format!("Prune {}?", target.raw_value()),
        }
    }

    pub fn confirmation_message(&self) -> String {
        match self {
            DockerOperation::Container { action: ContainerAction::Kill { .. }, name, .. } => {
                format!("Send SIGKILL to '{name}'? The process gets no chance to shut down cleanly.")
            }
            DockerOperation::Container { action, name, .. } => format!("{} '{name}'?", capitalized_first(action.verb())),
            DockerOperation::RemoveContainer { name, force: true, .. } => {
                format!("Force-remove '{name}'? It will be killed if running. Its anonymous volumes are kept.")
            }
            DockerOperation::RemoveContainer { name, .. } => format!("Remove '{name}'? Its anonymous volumes are kept."),
            DockerOperation::Compose { operation: ComposeOperation::Down, project } => format!(
                "Run 'docker compose down' for '{project}'? Containers and networks of the project are removed; named volumes are kept."
            ),
            DockerOperation::Compose { operation, project } => format!("Run compose {} for '{project}'?", operation.raw_value()),
            DockerOperation::RemoveImage { name, force: true, .. } => format!("Force-remove image '{name}' even if containers use it?"),
            DockerOperation::RemoveImage { name, .. } => format!("Remove image '{name}'?"),
            DockerOperation::RemoveNetwork { name, .. } => format!("Remove network '{name}'?"),
            DockerOperation::RemoveVolume { name } => format!("Remove volume '{name}'? Its data is deleted permanently."),
            DockerOperation::Prune(PruneTarget::Containers) => "Remove all stopped containers?".into(),
            DockerOperation::Prune(PruneTarget::Images) => "Remove all dangling (untagged) images?".into(),
            DockerOperation::Prune(PruneTarget::Networks) => "Remove all networks not used by any container?".into(),
            DockerOperation::Prune(PruneTarget::Volumes) => {
                "Remove all unused anonymous volumes? Their data is deleted permanently.".into()
            }
        }
    }

    /// Shown while the operation runs.
    pub fn progress_description(&self) -> String {
        match self {
            DockerOperation::Container { action, name, .. } => {
                format!("{} {name}…", progress_verb(action))
            }
            DockerOperation::RemoveContainer { name, .. } => format!("Removing {name}…"),
            DockerOperation::Compose { operation, project } => {
                format!("compose {} {project}…", operation.raw_value())
            }
            DockerOperation::RemoveImage { name, .. } => format!("Removing image {name}…"),
            DockerOperation::RemoveNetwork { name, .. } => format!("Removing network {name}…"),
            DockerOperation::RemoveVolume { name } => format!("Removing volume {name}…"),
            DockerOperation::Prune(target) => format!("Pruning {}…", target.raw_value()),
        }
    }

    pub fn success_description(&self) -> String {
        match self {
            DockerOperation::Container { action, name, .. } => {
                format!("{} {name}", past_verb(action))
            }
            DockerOperation::RemoveContainer { name, .. } => format!("Removed {name}"),
            DockerOperation::Compose { operation, project } => {
                format!("compose {} {project} finished", operation.raw_value())
            }
            DockerOperation::RemoveImage { name, .. } => format!("Removed image {name}"),
            DockerOperation::RemoveNetwork { name, .. } => format!("Removed network {name}"),
            DockerOperation::RemoveVolume { name } => format!("Removed volume {name}"),
            DockerOperation::Prune(target) => format!("Pruned {}", target.raw_value()),
        }
    }

    /// Whether images/networks/volumes lists should refresh too.
    pub fn touches_inventory(&self) -> bool {
        !matches!(self, DockerOperation::Container { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationResult {
    pub operation: DockerOperation,
    pub succeeded: bool,
    pub message: String,
}
