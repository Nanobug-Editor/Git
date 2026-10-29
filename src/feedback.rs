//! Typed operation feedback is delivered through the host's shared notification/history owner.
use super::*;
use plugin_protocol::interaction::{Operation, Severity};

/// Message delivery has its own bounded lifetime and never owns Git's business busy state.
#[derive(Default)]
pub(super) struct Feedback {
    /// Every distinct report in this turn survives later failures in the same scene publication.
    current: Vec<(Severity, String)>,
    /// Waiting messages retain severity chosen by their originating operation.
    pending: VecDeque<(Severity, String)>,
    /// Fast publication receipts are correlated separately from document/command requests.
    active: Vec<Delivery>,
}

/// Retain the original cause until the host confirms publication, including failed terminal receipts.
struct Delivery {
    handle: api::ResourceHandle,
    severity: Severity,
    message: String,
}

impl Feedback {
    /// Record a typed message while another disjoint business field is mutably borrowed.
    pub(super) fn record(&mut self, severity: Severity, message: &str) {
        self.current.push((severity, message.into()));
    }
}

impl State {
    /// Retain the latest operation state and queue every explicit diagnostic for host publication.
    pub(super) fn report(&mut self, severity: Severity, message: String) {
        self.message = message;
        self.feedback.record(severity, &self.message);
    }

    /// Collect diagnostics created during this turn, once even when several state fields mirror them.
    pub(super) fn collect_feedback(&mut self, previous_error: &str, previous_submit: Option<&str>) {
        let mut messages = std::mem::take(&mut self.feedback.current);
        if self.history_error != previous_error && !self.history_error.is_empty() {
            messages.push((Severity::Error, self.history_error.clone()));
        }
        if self.commit_submit_error.as_deref() != previous_submit
            && let Some(error) = &self.commit_submit_error
        {
            messages.push((Severity::Error, error.clone()));
        }
        let mut emitted = Vec::new();
        for (severity, message) in messages {
            let message = safe_diagnostic(message.as_bytes());
            if message.trim().is_empty() || emitted.contains(&message) {
                continue;
            }
            // Mirrored fields are one report; a repeated failure on a later action remains a new log.
            emitted.push(message.clone());
            let mut rest = message.as_str();
            while !rest.is_empty() {
                // Preserve the complete bounded diagnostic across UTF-8-safe interaction chunks.
                let mut end = rest.len().min(4096);
                while !rest.is_char_boundary(end) {
                    end -= 1;
                }
                if self.feedback.pending.len() == 32
                    && let Some((severity, message)) = self.feedback.pending.pop_front()
                {
                    delivery_fallback(severity, &message, "notification queue is busy");
                }
                self.feedback
                    .pending
                    .push_back((severity, rest[..end].into()));
                rest = &rest[end..];
            }
        }
    }

    /// Publish bounded UTF-8 messages using `start`; failure leaves causes available for fallback.
    pub(super) fn flush_feedback(
        &mut self,
        mut start: impl FnMut(Operation, u32) -> Result<api::ResourceHandle, api::Failure>,
    ) -> Result<(), api::Failure> {
        while self.feedback.active.len() < 8 {
            let Some((severity, message)) = self.feedback.pending.front() else {
                break;
            };
            let handle = start(
                Operation::PublishMessage {
                    title: "Git".into(),
                    message: message.clone(),
                    severity: *severity,
                },
                30_000,
            )?;
            self.feedback.active.push(Delivery {
                handle,
                severity: *severity,
                message: message.clone(),
            });
            self.feedback.pending.pop_front();
        }
        Ok(())
    }

    /// Keep successful business output independent from notice admission and preserve every queued cause.
    pub(super) fn feedback_failed(&mut self, error: &api::Failure) {
        let reason = safe_diagnostic(error.to_string().as_bytes());
        for (severity, message) in self.feedback.pending.drain(..) {
            delivery_fallback(severity, &message, &reason);
        }
    }

    /// Consume only exact notification handles; late completion cannot repaint or restart Git work.
    pub(super) fn observe_feedback(&mut self, event: &api::Notification) -> bool {
        let api::Notification::Request { handle, update } = event else {
            return false;
        };
        let Some(index) = self
            .feedback
            .active
            .iter()
            .position(|owned| &owned.handle == handle)
        else {
            return false;
        };
        if matches!(
            update,
            api::RequestUpdate::Completed { .. } | api::RequestUpdate::Cancelled { .. }
        ) {
            let delivery = self.feedback.active.remove(index);
            match update {
                api::RequestUpdate::Completed {
                    result:
                        Ok(api::EditorValue::Interaction(
                            plugin_protocol::interaction::Value::Published,
                        )),
                } => {}
                api::RequestUpdate::Completed { result: Err(error) } => delivery_fallback(
                    delivery.severity,
                    &delivery.message,
                    &safe_diagnostic(error.to_string().as_bytes()),
                ),
                api::RequestUpdate::Cancelled { reason, .. } => delivery_fallback(
                    delivery.severity,
                    &delivery.message,
                    &format!("publication cancelled: {reason:?}"),
                ),
                api::RequestUpdate::Completed { result: Ok(_) } => delivery_fallback(
                    delivery.severity,
                    &delivery.message,
                    "unexpected message publication response",
                ),
                _ => {}
            }
            // Terminal editor receipts already retire their request authority in the host.
        }
        true
    }
}

/// A delivery failure is an error itself; the original typed severity remains explicit in the log.
fn delivery_fallback(severity: Severity, message: &str, reason: &str) {
    eprintln!(
        "Git 消息发布失败 / Git message publication failed ({reason}); original severity={severity:?}: {message}"
    );
}

#[cfg(test)]
/// Native unit tests replace only the notice transport, preserving real queue and routing logic.
pub(super) fn accept(operation: Operation, _: u32) -> Result<api::ResourceHandle, api::Failure> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NOTICES.with(|notices| notices.borrow_mut().push(operation));
    Ok(api::ResourceHandle {
        instance: "test-feedback".into(),
        scope: "workspace".into(),
        resource: NEXT.fetch_add(1, Ordering::Relaxed),
    })
}

#[cfg(test)]
thread_local! {
    /// Each native test observes exactly the notices emitted on its own dispatch thread.
    static NOTICES: RefCell<Vec<Operation>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
/// Drain captured public operations so tests assert host publication rather than internal fields.
pub(super) fn take_notices() -> Vec<Operation> {
    NOTICES.with(|notices| std::mem::take(&mut *notices.borrow_mut()))
}

#[cfg(test)]
/// Exercise the production dispatcher while substituting the unavailable native notice import.
pub(super) fn dispatch(payload: String) -> Result<String, String> {
    dispatch_with(payload, Command::launch, accept)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A commit's real failed completion must not publish both stderr and its cleanup wrapper.
    #[test]
    fn single_cause_failed_commit_or_push_publishes_one_complete_diagnostic() {
        for purpose in [Purpose::Action, Purpose::PushAfterCommit { next: None }] {
            let push = matches!(purpose, Purpose::PushAfterCommit { .. });
            let mut state = State {
                commit_submitting: true,
                ..Default::default()
            };
            state
                .finished(
                    Running {
                        handle: api::ResourceHandle {
                            instance: "commit-error-test".into(),
                            scope: "workspace".into(),
                            resource: 1,
                        },
                        command: Command::new([if push { "push" } else { "commit" }], purpose),
                        output: Vec::new(),
                        stderr: b"fatal: index.lock already exists".to_vec(),
                    },
                    128,
                )
                .unwrap();
            state.collect_feedback("", None);
            let _ = take_notices();
            state.flush_feedback(accept).unwrap();
            let notices = take_notices();
            assert_eq!(
                notices.len(),
                1,
                "push={push}: one failed operation has one final diagnostic"
            );
            let Operation::PublishMessage {
                severity: Severity::Error,
                message,
                ..
            } = &notices[0]
            else {
                panic!("typed failure");
            };
            assert!(message.contains("index.lock"));
            assert_eq!(state.commit_submit_error.as_deref(), Some(message.as_str()));
            if push {
                assert!(message.contains("local commit retained"));
            }
        }
    }

    /// A synchronous push launch rejection has the same single final-cause contract as async failure.
    #[test]
    fn single_cause_failed_push_launch_publishes_one_complete_diagnostic() {
        let mut state = State {
            commit_submitting: true,
            ..Default::default()
        };
        state.queue.push_back(Command::new(
            ["push"],
            Purpose::PushAfterCommit { next: None },
        ));
        assert!(state.pump_with(|_, _| Err(api::Failure::new(
            api::ErrorCode::PermissionDenied,
            "native launch rejected"
        ))));
        state.collect_feedback("", None);
        let _ = take_notices();
        state.flush_feedback(accept).unwrap();
        let notices = take_notices();
        assert_eq!(notices.len(), 1);
        assert!(
            matches!(&notices[0], Operation::PublishMessage { severity: Severity::Error, message, .. }
            if message.contains("native launch rejected") && message.contains("local commit retained"))
        );
    }

    /// Independent failures in the same publication turn each keep their original typed cause.
    #[test]
    fn multiple_reports_in_one_turn_preserve_every_distinct_diagnostic() {
        let mut state = State::default();
        state.report(Severity::Error, "workspace publication rejected".into());
        state.report(
            Severity::Warning,
            "branch metadata publication rejected".into(),
        );
        state.collect_feedback("", None);
        let _ = take_notices();
        state.flush_feedback(accept).unwrap();
        let notices = take_notices();
        assert_eq!(notices.len(), 2);
        assert!(
            matches!(&notices[0], Operation::PublishMessage { severity: Severity::Error, message, .. } if message == "workspace publication rejected")
        );
        assert!(
            matches!(&notices[1], Operation::PublishMessage { severity: Severity::Warning, message, .. } if message == "branch metadata publication rejected")
        );
    }

    /// Burst publication reserves bounded authority, and only an exact terminal receipt frees a slot.
    #[test]
    fn burst_feedback_is_bounded_and_drains_on_exact_publication_receipts() {
        let mut state = State::default();
        for index in 0..40 {
            state.report(
                Severity::Warning,
                format!("operation {index} requires attention"),
            );
            state.collect_feedback("", None);
            state.flush_feedback(accept).unwrap();
        }
        assert_eq!(state.feedback.active.len(), 8);
        assert_eq!(state.feedback.pending.len(), 32);
        assert!(!state.busy());
        let handle = state.feedback.active[0].handle.clone();
        let mut foreign = handle.clone();
        foreign.instance = "another-instance".into();
        let completed = |handle| api::Notification::Request {
            handle,
            update: api::RequestUpdate::Completed {
                result: Ok(api::EditorValue::Interaction(
                    plugin_protocol::interaction::Value::Published,
                )),
            },
        };
        assert!(!state.observe_feedback(&completed(foreign)));
        assert_eq!(state.feedback.pending.len(), 32);
        assert!(state.observe_feedback(&completed(handle)));
        state.flush_feedback(accept).unwrap();
        assert_eq!(state.feedback.active.len(), 8);
        assert_eq!(state.feedback.pending.len(), 31);
        assert_eq!(state.revision, 0);
    }

    /// Collect each typed source and assert the public operation, including complete Unicode chunks.
    #[test]
    fn messages_preserve_explicit_levels_complete_unicode_and_redact_credentials() {
        let mut state = State::default();
        let original = format!(
            "https://user:secret@example.invalid/{}",
            "中文😀".repeat(900)
        );
        for (severity, message) in [
            (Severity::Information, "operation complete".into()),
            (Severity::Warning, "metadata subscription ended".into()),
            (Severity::Error, original.clone()),
        ] {
            state.report(severity, message);
            state.collect_feedback("", None);
        }
        let _ = take_notices();
        state.flush_feedback(accept).unwrap();
        let notices = take_notices();
        assert!(matches!(
            &notices[0],
            Operation::PublishMessage {
                severity: Severity::Information,
                ..
            }
        ));
        assert!(matches!(
            &notices[1],
            Operation::PublishMessage {
                severity: Severity::Warning,
                ..
            }
        ));
        let mut diagnostic = String::new();
        for notice in &notices[2..] {
            let Operation::PublishMessage {
                severity: Severity::Error,
                message,
                ..
            } = notice
            else {
                panic!("typed error");
            };
            assert!(message.len() <= 4096);
            diagnostic.push_str(message);
        }
        assert_eq!(diagnostic, safe_diagnostic(original.as_bytes()));
        assert!(!diagnostic.contains("secret"));
        assert!(!state.busy());
    }

    /// A long diagnostic mirrored into history must retain its explicit warning level exactly once.
    #[test]
    fn mirrored_unicode_diagnostics_keep_explicit_severity_without_duplicate_chunks() {
        let mut state = State::default();
        let text = "诊断文本".repeat(900);
        state.report(Severity::Warning, text.clone());
        state.history_error = text.clone();
        state.collect_feedback("", None);
        let _ = take_notices();
        state.flush_feedback(accept).unwrap();
        let mut published = String::new();
        for operation in take_notices() {
            let Operation::PublishMessage {
                message,
                severity: Severity::Warning,
                ..
            } = operation
            else {
                panic!("mirrored fields must not replace the operation's explicit severity");
            };
            published.push_str(&message);
        }
        assert_eq!(published, text);
    }

    /// A notification response only releases its own slot; no business view, query or revision changes.
    #[test]
    fn notification_completion_keeps_business_scene_and_busy_state_unchanged() {
        let mut state = State {
            revision: 12,
            ..Default::default()
        };
        state.report(
            Severity::Warning,
            "stream recovered on next selection".into(),
        );
        state.collect_feedback("", None);
        state.flush_feedback(accept).unwrap();
        let handle = state.feedback.active[0].handle.clone();
        STATE.with(|cell| *cell.borrow_mut() = state);
        let invocation = api::Invocation {
            id: 1,
            message: api::Input::Event {
                panel: None,
                event: api::Notification::Request {
                    handle,
                    update: api::RequestUpdate::Completed {
                        result: Ok(api::EditorValue::Interaction(
                            plugin_protocol::interaction::Value::Published,
                        )),
                    },
                },
            },
        };
        let completion: api::Completion = serde_json::from_str(
            &dispatch_with(
                serde_json::to_string(&invocation).unwrap(),
                |_, _| panic!("message receipt cannot launch Git"),
                accept,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(completion.result.unwrap().views.is_empty());
        STATE.with(|cell| {
            let state = std::mem::take(&mut *cell.borrow_mut());
            assert_eq!(state.revision, 12);
            assert!(!state.busy());
            assert!(state.feedback.active.is_empty());
        });
    }

    /// Missing interaction authority cannot turn accepted business scenes into an undelivered failure.
    #[test]
    fn message_delivery_failure_preserves_successful_business_publication() {
        STATE.with(|cell| {
            let mut state = State::default();
            state.report(Severity::Error, "original operation diagnostic".into());
            *cell.borrow_mut() = state;
        });
        let invocation = api::Invocation {
            id: 1,
            message: api::Input::Event {
                panel: None,
                event: api::Notification::Theme(Environment::default()),
            },
        };
        let completion: api::Completion = serde_json::from_str(
            &dispatch_with(
                serde_json::to_string(&invocation).unwrap(),
                |_, _| panic!("theme change cannot launch Git"),
                |_, _| {
                    Err(api::Failure::new(
                        api::ErrorCode::PermissionDenied,
                        "notice permission denied",
                    ))
                },
            )
            .unwrap(),
        )
        .unwrap();
        let output = completion
            .result
            .expect("notice transport must not discard business output");
        assert!(!output.views.is_empty());
        STATE.with(|cell| {
            let state = std::mem::take(&mut *cell.borrow_mut());
            assert!(
                state.feedback.pending.is_empty(),
                "fallback drained every cause to the host log"
            );
            assert_eq!(state.published_views.len(), output.views.len());
        });
    }

    /// Scope-limited typed commands require the same declared interaction grant as ambient UI actions.
    #[test]
    fn manifest_delegates_interaction_to_each_typed_git_command() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../manifest.json")).unwrap();
        assert!(
            manifest["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == "ui.interaction")
        );
        for command in manifest["commands"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|command| command.get("signature").is_some())
        {
            assert!(
                command["signature"]["permissions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item == "ui.interaction"),
                "{}",
                command["id"]
            );
        }
    }
}
