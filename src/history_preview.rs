//! An immutable commit preview shares the history dock; opening a file is a separate editor action.
use super::*;

/// Preserve a selected commit's complete UTF-8 message within the native document's bounded quota.
pub(super) fn decode_message(bytes: Vec<u8>) -> Result<String, api::Failure> {
    if bytes.len() > 256 * 1024 || bytes.contains(&0) {
        return Err(failure(
            "提交说明超出预览限制 / Commit message exceeds the preview limit",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| failure("提交说明不是 UTF-8 / Commit message is not UTF-8"))
}

impl State {
    /// Stop failed preview work and retain its diagnostic for the shared host message publication.
    pub(super) fn fail_history_preview(&mut self) {
        if self.history_loading || self.diff_history {
            self.history_error = self.message.clone();
            self.history_loading = false;
            self.pending_history_files = None;
            self.diff_history = false;
            self.diff_mode = None;
        }
    }
    /// Show file navigation above metadata; both native panes retain their independent scroll state.
    pub(super) fn history_preview(&self) -> ui::Node {
        let mut details = Vec::new();
        if let Some(commit) = self
            .history_preview_commit
            .as_ref()
            .or_else(|| self.history_commit())
        {
            details.push(ui::Node::text("history-subject", &commit.title));
            details.push(ui::Node::text("history-oid", &commit.oid));
            details.push(ui::Node::text(
                "history-author",
                format!("{}: {}", self.label("提交人", "Author"), commit.author),
            ));
            details.push(ui::Node::text(
                "history-time",
                format!("{}: {}", self.label("提交时间", "Commit time"), commit.time),
            ));
            details.push(ui::Node::text(
                "history-parents",
                format!(
                    "{}: {}",
                    self.label("父提交", "Parents"),
                    if commit.parents.is_empty() {
                        self.label("空树", "Empty tree").into()
                    } else {
                        commit.parents.join("\n")
                    }
                ),
            ));
            details.push(self.history_parent_controls());
            // Per-node text is bounded to 64 KiB. Splitting at UTF-8 boundaries preserves the
            // complete message and paragraph breaks without truncating a legitimate commit.
            let heading = format!("{}\n", commit.title);
            let mut rest = self
                .history_message
                .strip_prefix(&heading)
                .unwrap_or(&self.history_message);
            let mut part = 0;
            while !rest.is_empty() {
                let mut end = rest.len().min(16000);
                while !rest.is_char_boundary(end) {
                    end -= 1;
                }
                details.push(ui::Node::text(
                    format!("history-message-{part}"),
                    &rest[..end],
                ));
                rest = &rest[end..];
                part += 1;
            }
        } else {
            details.push(ui::Node::text(
                "history-empty",
                self.label("选择提交查看详情", "Select a commit to review its details"),
            ));
        }
        if self.history_loading {
            details.push(ui::Node::text(
                "history-loading",
                self.label("正在加载提交…", "Loading commit…"),
            ));
        }
        if self.history_cache.pending {
            details.push(ui::Node::text(
                "history-filter-loading",
                format!(
                    "{} {}",
                    self.label("正在加载分支…", "Loading branch…"),
                    self.filter.as_deref().unwrap_or("--all")
                ),
            ));
        }
        // Operation diagnostics are published to the host message owner; commit metadata stays stable.
        ui::Node::column(
            "history-preview",
            vec![
                ui::Node::column(
                    "history-files-pane",
                    vec![
                        // Keep the last admitted tree visible. Busy/source checks reject old
                        // gestures until files and the message of the new OID are ready together.
                        self.tree_node("files").disabled(
                            self.history_loading
                                || self
                                    .history_preview_commit
                                    .as_ref()
                                    .is_some_and(|commit| commit.oid != self.selected),
                        ),
                    ],
                )
                .grow()
                .padding(8.),
                ui::Node::scroll(
                    "history-details-scroll",
                    ui::Node::column("history-details", details).padding(8.),
                )
                .grow(),
            ],
        )
        .resizable()
        .width(340.)
        .grow()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// First load seeds the selected row and its immutable preview once; later pages retain choice.
    #[test]
    fn first_page_selects_preview_without_reselecting_on_refresh() {
        let mut state = State::default();
        let first = Commit {
            oid: "first".into(),
            title: "First".into(),
            ..Default::default()
        };
        let second = Commit {
            oid: "second".into(),
            title: "Second".into(),
            ..Default::default()
        };
        state.history_page(vec![first.clone(), second.clone()]);
        assert_eq!(state.selected, "first");
        assert_eq!(state.history_cursor, "first");
        assert_eq!(state.selected_commits, ["first"]);
        assert!(state.history_loading);
        assert!(
            matches!(&state.queue.front().unwrap().purpose, Purpose::Files(oid) if oid == "first")
        );
        state.queue.clear();
        state.select_history_row(1, false);
        state.queue.clear();
        state.offset = 0;
        state.history_page(vec![first, second]);
        assert_eq!(state.selected, "second");
        assert!(state.queue.is_empty());
    }

    /// Invalid/binary/oversized messages produce an explicit preview failure, never partial text.
    #[test]
    fn complete_message_is_bounded_and_utf8_without_rewriting_paragraphs() {
        let message = "feat: 中文预览\n\n保留完整说明。\n\nReviewed-by: 演示者\n";
        assert_eq!(
            decode_message(message.as_bytes().to_vec()).unwrap(),
            message
        );
        for bytes in [vec![0], vec![255], vec![b'a'; 256 * 1024 + 1]] {
            assert!(decode_message(bytes).is_err());
        }
    }

    /// Large Unicode messages remain complete in bounded native text nodes, in both locales.
    #[test]
    fn preview_is_a_split_dock_with_complete_selected_metadata_and_message() {
        for locale in ["zh-CN", "en-US"] {
            let message = "完整说明\n".repeat(6000);
            let mut state = State {
                selected: "a".repeat(40),
                history_message: message.clone(),
                history_selection: Some(Commit {
                    oid: "a".repeat(40),
                    title: "提交标题".into(),
                    author: "作者".into(),
                    time: "2026-10-19T12:30:00+08:00".into(),
                    ..Default::default()
                }),
                ..Default::default()
            };
            state.environment.locale = locale.into();
            let preview = state.history_preview();
            assert!(preview.layout.resizable);
            let mut paragraphs = String::new();
            preview.visit(&mut |node| {
                if node.id.starts_with("history-message-") {
                    let ui::Kind::Text { text } = &node.kind else {
                        panic!("message text")
                    };
                    assert!(text.len() <= 16000);
                    paragraphs.push_str(text);
                }
            });
            assert_eq!(paragraphs, message);
        }
    }

    /// Late file/message completions cannot replace a newer selection or summon a details window.
    #[test]
    fn files_and_message_receipts_only_publish_the_selected_inline_preview() {
        let mut state = State {
            selected: "new".into(),
            history_loading: true,
            ..Default::default()
        };
        let complete = |state: &mut State, purpose: Purpose, output: &[u8]| {
            state
                .finished(
                    Running {
                        handle: api::ResourceHandle {
                            instance: "preview".into(),
                            scope: "workspace".into(),
                            resource: 1,
                        },
                        command: Command::new(["query"], purpose),
                        output: output.to_vec(),
                        stderr: Vec::new(),
                    },
                    0,
                )
                .unwrap();
        };
        complete(&mut state, Purpose::Files("old".into()), b"old.txt\0");
        complete(
            &mut state,
            Purpose::CommitMessage("old".into()),
            b"old message\n",
        );
        assert!(state.history_files.is_empty());
        assert!(state.history_message.is_empty());
        assert!(state.queue.is_empty());
        complete(
            &mut state,
            Purpose::Files("new".into()),
            b"A\0src/new.rs\0M\0src/new.rs\0A\0README.md\0",
        );
        assert!(
            state.history_files.is_empty(),
            "file metadata waits for the matching complete message"
        );
        assert!(
            matches!(state.queue.pop_front().unwrap().purpose, Purpose::CommitMessage(oid) if oid == "new")
        );
        complete(
            &mut state,
            Purpose::CommitMessage("new".into()),
            b"new message\n\nfull body\n",
        );
        assert_eq!(state.history_message, "new message\n\nfull body\n");
        assert_eq!(state.history_files, ["README.md", "src/new.rs"]);
        assert!(!state.history_loading);
        assert!(
            state.editor_tasks.is_empty(),
            "selection does not request native window visibility"
        );
    }
}
