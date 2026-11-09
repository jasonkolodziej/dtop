//! What to do with the followed log stream when the selection, pane, host or the stream itself
//! changes. Pure, so the rules are tested without a terminal.

use dtop_core::logstream::{LogLine, LogStreamKind};
use dtop_core::timestamp::Timestamp;

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

/// Where a resumed stream picks up: the last shown line's exact timestamp, and the lines shown
/// at that very instant. Docker's `since` is inclusive, so the resumed stream repeats those
/// boundary lines; each is skipped once, while other lines that share the instant (written
/// after the stream dropped) are kept.
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeCursor {
    pub after: Timestamp,
    /// Shown lines stamped exactly `after`, oldest first: (stream, text).
    seen: Vec<(LogStreamKind, String)>,
}

impl ResumeCursor {
    /// The cursor for lines already on screen, or None when the last one has no timestamp.
    pub fn from_shown(lines: &[LogLine]) -> Option<ResumeCursor> {
        let after = lines.last()?.timestamp?;
        let mut seen: Vec<(LogStreamKind, String)> = lines
            .iter()
            .rev()
            .take_while(|l| l.timestamp == Some(after))
            .map(|l| (l.stream, l.text.clone()))
            .collect();
        seen.reverse();
        Some(ResumeCursor { after, seen })
    }

    /// The lines of a resumed batch that are new. Stateful across batches: a boundary line is
    /// skipped only once. Lines without a timestamp are kept.
    pub fn new_lines(&mut self, batch: Vec<LogLine>) -> Vec<LogLine> {
        batch
            .into_iter()
            .filter(|line| match line.timestamp {
                None => true,
                Some(t) if t < self.after => false,
                Some(t) if t == self.after => {
                    match self
                        .seen
                        .iter()
                        .position(|(stream, text)| *stream == line.stream && *text == line.text)
                    {
                        Some(index) => {
                            self.seen.remove(index);
                            false
                        }
                        None => true,
                    }
                }
                Some(_) => {
                    // Past the boundary: nothing more of it can arrive.
                    self.seen.clear();
                    true
                }
            })
            .collect()
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
        let at = |nanos| Timestamp {
            seconds: 1_714_566_896,
            nanos,
        };
        let line = |t: Option<Timestamp>, text: &str| LogLine {
            stream: LogStreamKind::Stdout,
            text: text.into(),
            timestamp: t,
        };
        let shown = vec![line(Some(at(1)), "a"), line(Some(at(2)), "b")];
        let mut cursor = ResumeCursor::from_shown(&shown).unwrap();
        let kept = cursor.new_lines(vec![
            line(Some(at(1)), "a"),
            line(Some(at(2)), "b"),
            line(Some(at(3)), "c"),
            line(None, "d"),
        ]);
        assert_eq!(
            kept.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec!["c", "d"]
        );
        assert_eq!(scroll_back(10, 5, 12), 12);
        assert_eq!(ResumeCursor::from_shown(&[line(None, "x")]), None);
    }

    #[test]
    fn lines_a_nanosecond_apart_or_at_the_same_instant_are_not_lost() {
        let at = |nanos| Timestamp {
            seconds: 1_714_566_896,
            nanos,
        };
        let line = |t: Timestamp, text: &str| LogLine {
            stream: LogStreamKind::Stdout,
            text: text.into(),
            timestamp: Some(t),
        };
        // One line shown before the stream dropped; the next was written 1 ns later.
        let mut cursor = ResumeCursor::from_shown(&[line(at(123_456_789), "first")]).unwrap();
        let kept = cursor.new_lines(vec![
            line(at(123_456_789), "first"),
            line(at(123_456_790), "second"),
        ]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text, "second");

        // Two lines share the boundary instant but only one was shown; the resumed stream
        // arrives in two batches.
        let mut cursor = ResumeCursor::from_shown(&[line(at(5), "x")]).unwrap();
        assert!(cursor.new_lines(vec![line(at(5), "x")]).is_empty());
        let kept = cursor.new_lines(vec![line(at(5), "x"), line(at(5), "y")]);
        assert_eq!(
            kept.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec!["x", "y"]
        );
    }
}
