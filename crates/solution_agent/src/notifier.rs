use std::time::{Duration, Instant};

use gpui::App;

use crate::model::{SessionState, SolutionSessionId};

pub const NOTIFICATION_THRESHOLD: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyKind {
    Completed,
    AwaitingInput,
    Errored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationDecision {
    pub kind: NotifyKind,
    pub elapsed: Duration,
    pub session_id: SolutionSessionId,
}

/// Pure decision function: given a previous and next `SessionState`, decides
/// whether the user should be notified about the transition. The 5-minute
/// gate suppresses notifications for fast turns; `Errored` always notifies
/// regardless of elapsed time. When the originating session is currently
/// focused in the UI, no notification fires.
pub fn decide_notification(
    session_id: SolutionSessionId,
    previous: &SessionState,
    next: &SessionState,
    now: Instant,
    is_focused: bool,
) -> Option<NotificationDecision> {
    let prev_started = match previous {
        SessionState::Running {
            started_at,
            notified: false,
        } => Some(*started_at),
        _ => None,
    };

    let kind = match (previous, next) {
        (SessionState::Running { .. }, SessionState::Idle) => NotifyKind::Completed,
        (SessionState::Running { .. }, SessionState::AwaitingInput) => NotifyKind::AwaitingInput,
        (_, SessionState::Errored(_)) => NotifyKind::Errored,
        _ => return None,
    };

    if is_focused {
        return None;
    }

    let elapsed = prev_started
        .map(|s| now.duration_since(s))
        .unwrap_or(Duration::ZERO);

    if matches!(kind, NotifyKind::Errored) || elapsed >= NOTIFICATION_THRESHOLD {
        Some(NotificationDecision {
            kind,
            elapsed,
            session_id,
        })
    } else {
        None
    }
}

/// Fire a desktop notification via the freedesktop portal (Linux/FreeBSD).
/// Other platforms log a warning placeholder until a per-platform backend
/// is added. Errors from the portal are intentionally swallowed: a broken
/// DBus session must not crash the editor.
pub fn dispatch(decision: &NotificationDecision, title: &str, body: &str, _cx: &mut App) {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        use ashpd::desktop::notification::{Notification, NotificationProxy, Priority};
        use util::ResultExt;

        let title = title.to_string();
        let body = body.to_string();
        let session_id_str = decision.session_id.to_string();
        let priority = if matches!(
            decision.kind,
            NotifyKind::Errored | NotifyKind::AwaitingInput
        ) {
            Priority::High
        } else {
            Priority::Normal
        };
        smol::spawn(async move {
            let Ok(proxy) = NotificationProxy::new().await else {
                return;
            };
            proxy
                .add_notification(
                    &format!("dev.spk-editor.session-{session_id_str}"),
                    Notification::new(&title)
                        .body(body.as_str())
                        .priority(priority),
                )
                .await
                .log_err();
        })
        .detach();
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        let _ = (title, body);
        log::warn!(
            "OS notifications not implemented for this platform; decision: {decision:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_notification_under_5_minutes() {
        let started = Instant::now();
        let now = started + Duration::from_secs(60 * 4);
        let prev = SessionState::Running {
            started_at: started,
            notified: false,
        };
        let next = SessionState::Idle;
        assert_eq!(
            decide_notification(SolutionSessionId::new(), &prev, &next, now, false),
            None
        );
    }

    #[test]
    fn notification_at_5_minutes_exact() {
        let started = Instant::now();
        let now = started + NOTIFICATION_THRESHOLD;
        let prev = SessionState::Running {
            started_at: started,
            notified: false,
        };
        let next = SessionState::Idle;
        let decision = decide_notification(SolutionSessionId::new(), &prev, &next, now, false);
        assert!(matches!(decision, Some(d) if d.kind == NotifyKind::Completed));
    }

    #[test]
    fn no_notification_when_focused() {
        let started = Instant::now();
        let now = started + Duration::from_secs(600);
        let prev = SessionState::Running {
            started_at: started,
            notified: false,
        };
        let next = SessionState::Idle;
        assert_eq!(
            decide_notification(SolutionSessionId::new(), &prev, &next, now, true),
            None
        );
    }

    #[test]
    fn errored_notifies_regardless_of_threshold() {
        let started = Instant::now();
        let now = started + Duration::from_secs(10);
        let prev = SessionState::Running {
            started_at: started,
            notified: false,
        };
        let next = SessionState::Errored("boom".into());
        let decision = decide_notification(SolutionSessionId::new(), &prev, &next, now, false);
        assert!(matches!(decision, Some(d) if d.kind == NotifyKind::Errored));
    }
}
