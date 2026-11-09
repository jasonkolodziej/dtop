//! What to do with the followed log stream when the selection, pane, host or the stream itself
//! changes. Pure, so the rules are tested without a terminal.

use dtop_core::logstream::LogLine;

use crate::state::LogStreamState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: usize,
    pub container_id: String,
    /// Only running containers produce new lines.
    pub is_active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The open stream already shows the right container.
    Keep,
    /// Close any open stream and start one for the target.
    Start,
    /// Close any open stream and leave it closed.
    Stop,
    /// Leave things as they are (an ended stream, or a reconnect already scheduled).
    Wait,
}

/// Lines of a resumed stream that are new. `since` is inclusive, so lines at or before the last
/// shown one are dropped; lines without a timestamp are kept.
pub fn new_lines(batch: Vec<LogLine>, after: Option<f64>) -> Vec<LogLine> {
    match after {
        None => batch,
        Some(last) => batch
            .into_iter()
            .filter(|l| l.timestamp.unwrap_or(f64::INFINITY) > last)
            .collect(),
    }
}

/// The scroll-back after `appended` lines arrive while scrolled back: it grows so the same lines
/// stay on screen, but never past the oldest line kept.
pub fn scroll_back(current: usize, appended: usize, kept_lines: usize) -> usize {
    (current + appended).min(kept_lines)
}

/// - `current`: the open stream, if any.
/// - `wanted`: the container the logs pane shows, or None when no logs are visible.
/// - `state`: the shown container's last stream state (None when its data is not loaded).
/// - `reconnect_pending`: a reconnect is scheduled for the shown container.
/// - `is_reconnect`: this call is that scheduled reconnect.
pub fn decide(
    current: Option<(usize, &str)>,
    wanted: Option<&Target>,
    state: Option<&LogStreamState>,
    reconnect_pending: bool,
    is_reconnect: bool,
) -> Decision {
    let Some(wanted) = wanted else {
        return if current.is_none() {
            Decision::Wait
        } else {
            Decision::Stop
        };
    };
    if let Some((host, id)) = current {
        if host == wanted.host && id == wanted.container_id {
            return Decision::Keep;
        }
    }
    if is_reconnect {
        return Decision::Start;
    }
    if current.is_none() && state.is_some() {
        // Same container as before: a stopped container's stream already delivered everything,
        // and a running one's reconnect is on its way.
        if matches!(state, Some(LogStreamState::Ended(_))) && !wanted.is_active {
            return Decision::Wait;
        }
        if reconnect_pending {
            return Decision::Wait;
        }
    }
    Decision::Start
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtop_core::logstream::LogStreamKind;

    fn target(active: bool) -> Target {
        Target {
            host: 0,
            container_id: "a".into(),
            is_active: active,
        }
    }

    #[test]
    fn keeps_matching_streams_and_stops_hidden_ones() {
        assert_eq!(
            decide(Some((0, "a")), Some(&target(true)), None, false, false),
            Decision::Keep
        );
        assert_eq!(
            decide(Some((0, "a")), None, None, false, false),
            Decision::Stop
        );
        assert_eq!(decide(None, None, None, false, false), Decision::Wait);
        assert_eq!(
            decide(Some((1, "a")), Some(&target(true)), None, false, false),
            Decision::Start
        );
    }

    #[test]
    fn stopped_containers_are_not_polled_and_reconnects_wait() {
        let ended = LogStreamState::Ended(None);
        assert_eq!(
            decide(None, Some(&target(false)), Some(&ended), false, false),
            Decision::Wait
        );
        assert_eq!(
            decide(None, Some(&target(true)), Some(&ended), true, false),
            Decision::Wait
        );
        assert_eq!(
            decide(None, Some(&target(true)), Some(&ended), true, true),
            Decision::Start
        );
        assert_eq!(
            decide(None, Some(&target(true)), Some(&ended), false, false),
            Decision::Start
        );
    }

    #[test]
    fn resumed_streams_drop_lines_already_shown() {
        let line = |t: Option<f64>| LogLine {
            stream: LogStreamKind::Stdout,
            text: String::new(),
            timestamp: t,
        };
        let kept = new_lines(
            vec![line(Some(1.0)), line(Some(2.0)), line(None)],
            Some(1.0),
        );
        assert_eq!(kept.len(), 2);
        assert_eq!(scroll_back(10, 5, 12), 12);
    }
}
