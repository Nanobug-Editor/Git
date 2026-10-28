//! Stash previews bind to immutable objects; destructive reflog actions recheck their mutable selectors.
use super::*;

impl State {
    /// Opening/paging the list is a local read. The primary action always applies while keeping it.
    pub(super) fn repository_stashes(&mut self, offset: usize) {
        // Keep one bounded page mounted; accumulating every visited page would exceed native UI quotas.
        self.repository.stashes.clear();
        self.repository.stash_selected = None;
        self.repository.stash_preview.clear();
        self.repository.stash_preview_ready = false;
        self.repository.stash_summary_only = false;
        self.repository.dialog = Some(Dialog::Stashes);
        self.queue.push_back(Command::new(
            vec![
                "stash".into(),
                "list".into(),
                "-z".into(),
                "--max-count=128".into(),
                format!("--skip={offset}"),
                "--format=%gd%x00%H%x00%gs%x00%ci".into(),
            ],
            Purpose::Repository(Query::StashList { offset }),
        ));
    }

    pub(super) fn repository_select_stash(&mut self, index: usize) -> Result<(), api::Failure> {
        let stash = self
            .repository
            .stashes
            .get(index)
            .cloned()
            .ok_or_else(|| failure("The stash list changed; refresh it"))?;
        self.repository.stash_selected = Some(stash.clone());
        self.repository.stash_preview_ready = false;
        self.repository.stash_preview.clear();
        self.repository.stash_truncated = false;
        self.repository.stash_summary_only = false;
        self.queue.push_back(Command::new(
            [
                "stash",
                "show",
                "--include-untracked",
                "--stat",
                "--summary",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--full-index",
                &stash.oid,
            ],
            Purpose::Repository(Query::StashPreview {
                oid: stash.oid.clone(),
            }),
        ));
        Ok(())
    }

    /// A button cannot apply a different list row while its preview belongs to an earlier OID.
    pub(super) fn repository_selected_stash(&self) -> Result<Stash, api::Failure> {
        if !self.repository.stash_preview_ready {
            return Err(failure(self.label(
                "先选择 Stash 并查看变更预览",
                "Select a stash and review its changes first",
            )));
        }
        self.repository
            .stash_selected
            .clone()
            .ok_or_else(|| failure("No stash selected"))
    }

    pub(super) fn repository_stash_apply(&mut self, drop_after: bool) -> Result<(), api::Failure> {
        let stash = self.repository_selected_stash()?;
        self.repository_apply_stash(stash, drop_after)
    }

    fn repository_apply_stash(
        &mut self,
        stash: Stash,
        drop_after: bool,
    ) -> Result<(), api::Failure> {
        // Applying an immutable object never relies on the reflog list position. If it conflicts,
        // the completion leaves the stash intact and reobserves Git's actual unmerged entries.
        self.enqueue_checked(Command::new(
            ["stash", "apply", &stash.oid],
            Purpose::Repository(Query::StashApplied {
                stash: stash.clone(),
                drop_after,
            }),
        ))
    }

    pub(super) fn repository_stash_confirm(&mut self) -> Result<(), api::Failure> {
        let Some(Dialog::StashConfirm { stash, pop }) = self.repository.dialog.clone() else {
            return Err(failure("The stash confirmation expired"));
        };
        self.repository.dialog = Some(Dialog::Stashes);
        if pop {
            self.repository_apply_stash(stash, true)
        } else {
            self.repository_verify_stash(stash, false);
            Ok(())
        }
    }

    fn repository_verify_stash(&mut self, stash: Stash, applied: bool) {
        self.queue.push_back(Command::new(
            vec![
                "rev-parse".into(),
                "--verify".into(),
                "--quiet".into(),
                format!("{}^{{commit}}", stash.selector),
            ],
            Purpose::Repository(Query::StashVerifySelector { stash, applied }),
        ));
    }

    pub(super) fn repository_stash_finished(
        &mut self,
        query: Query,
        output: Vec<u8>,
        stderr: Vec<u8>,
        code: u32,
    ) -> Result<(), api::Failure> {
        if code != 0 {
            self.repository_error(&stderr);
            if matches!(query, Query::StashApplied { .. }) {
                self.message = format!(
                    "{}\n{}",
                    self.message,
                    self.label(
                        "Stash 已保留；检查冲突后再继续",
                        "The stash was retained; inspect conflicts before continuing"
                    )
                );
                self.refresh();
            } else if let Query::StashVerifySelector { applied, .. } = query {
                self.message = self.label(
                    if applied { "内容已应用，但 Stash 列表已变化；保留记录，请刷新后决定是否删除" } else { "Stash 列表已变化；未删除任何记录，请刷新" },
                    if applied { "Changes were applied, but the stash list changed; the entry was kept. Refresh before deleting it" } else { "The stash list changed; no entry was deleted. Refresh it" },
                ).into();
                self.repository_stashes(0);
            }
            return Ok(());
        }
        match query {
            Query::StashList { offset } => {
                let page = model::stashes(&output).map_err(failure)?;
                self.repository.stash_more = page.len() == 128;
                self.repository.stash_cursor = offset + page.len();
                self.repository.stash_offset = offset;
                self.repository.stashes = page;
                if self.repository.stash_selected.is_none() && !self.repository.stashes.is_empty() {
                    let index = self
                        .repository
                        .stashes
                        .iter()
                        .position(|stash| {
                            stash.selector == self.argument.trim()
                                || stash.oid == self.argument.trim()
                        })
                        .unwrap_or(0);
                    self.repository_select_stash(index)?;
                }
            }
            Query::StashPreview { oid } => {
                if self
                    .repository
                    .stash_selected
                    .as_ref()
                    .is_some_and(|stash| stash.oid == oid)
                {
                    let (text, truncated) = model::preview(&output, 48 * 1024);
                    self.repository.stash_preview = text;
                    self.repository.stash_truncated = truncated;
                    self.repository.stash_preview_ready = true;
                    // The summary remains useful if a subsequent full patch exceeds transport
                    // limits. Applying always uses the complete Git object, never preview bytes.
                    self.repository.stash_summary_only = true;
                    self.queue.push_back(Command::new(
                        [
                            "stash",
                            "show",
                            "--include-untracked",
                            "--patch",
                            "--stat",
                            "--no-ext-diff",
                            "--no-textconv",
                            "--no-color",
                            "--full-index",
                            &oid,
                        ],
                        Purpose::Repository(Query::StashPatch { oid: oid.clone() }),
                    ));
                }
            }
            Query::StashPatch { oid } => {
                if self
                    .repository
                    .stash_selected
                    .as_ref()
                    .is_some_and(|stash| stash.oid == oid)
                {
                    let (text, truncated) = model::preview(&output, 48 * 1024);
                    self.repository.stash_preview = text;
                    self.repository.stash_truncated = truncated;
                    self.repository.stash_summary_only = false;
                }
            }
            Query::StashCreatePreview => {
                let changes = crate::model::status(&output).map_err(failure)?;
                if changes.iter().any(|change| change.conflict) {
                    return Err(failure(self.label(
                        "先处理未合并文件，不能创建 Stash",
                        "Resolve unmerged files before creating a stash",
                    )));
                }
                if changes.is_empty() {
                    self.message = self
                        .label("没有可存储的修改", "There are no changes to stash")
                        .into();
                } else {
                    self.repository.create_files = changes;
                    self.repository.create_status = output;
                    self.repository.create_page = 0;
                    self.repository.dialog = Some(Dialog::StashCreate);
                }
            }
            Query::StashCreateVerify { expected } => {
                if output != expected {
                    return Err(failure(self.label(
                        "文件状态已变化；请重新预览 Stash",
                        "File status changed; review the stash again",
                    )));
                }
                self.enqueue_checked(Command::new(
                    ["stash", "push", "--include-untracked"],
                    Purpose::Repository(Query::StashCreated),
                ))?;
            }
            Query::StashCreated => {
                self.message = self
                    .label(
                        "Stash 已创建，包含未跟踪文件，排除忽略文件",
                        "Stash created with untracked files; ignored files were excluded",
                    )
                    .into();
                self.refresh();
                self.repository_stashes(0);
            }
            Query::StashApplied { stash, drop_after } => {
                if drop_after {
                    self.repository_verify_stash(stash, true);
                } else {
                    self.message = self
                        .label("已应用并保留 Stash", "Stash applied and retained")
                        .into();
                    self.refresh();
                }
            }
            Query::StashVerifySelector { stash, applied } => {
                if model::oid(&output).map_err(failure)? != stash.oid {
                    self.message = self.label(
                        if applied { "已应用内容；Stash 位置已变化，保留记录，请刷新后再删除" } else { "Stash 位置已变化；未删除记录，请刷新" },
                        if applied { "Changes applied; the stash moved and was retained. Refresh before deleting it" } else { "The stash moved; no entry was deleted. Refresh it" },
                    ).into();
                    self.repository_stashes(0);
                } else {
                    // Git owns its reflog lock. This immediately preceding selector check rejects
                    // observed external changes; application never deletes an entry after failure.
                    self.queue.push_back(Command::new(
                        ["stash", "drop", &stash.selector],
                        Purpose::Repository(Query::StashDropped),
                    ));
                }
            }
            Query::StashDropped => {
                self.message = self.label("Stash 记录已删除", "Stash entry deleted").into();
                self.refresh();
                self.repository_stashes(0);
            }
            _ => unreachable!("non-stash query is routed by repository_finished"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stash() -> Stash {
        Stash {
            selector: "stash@{0}".into(),
            oid: "a".repeat(40),
            title: "saved changes".into(),
            time: String::new(),
        }
    }

    #[test]
    fn list_selection_previews_the_immutable_object_not_its_reflog_position() {
        let mut state = State::default();
        let entry = stash();
        let output = format!("{}\0{}\0{}\0date\0", entry.selector, entry.oid, entry.title);
        state
            .repository_stash_finished(
                Query::StashList { offset: 0 },
                output.into_bytes(),
                Vec::new(),
                0,
            )
            .unwrap();
        assert_eq!(state.queue.front().unwrap().args.last(), Some(&entry.oid));
        assert!(!state.repository.stash_preview_ready);
    }

    #[test]
    fn a_failed_pop_application_never_reaches_the_delete_step() {
        let mut state = State::default();
        state.repository.stashes.push(stash());
        state
            .repository_stash_finished(
                Query::StashApplied {
                    stash: stash(),
                    drop_after: true,
                },
                Vec::new(),
                b"CONFLICT".to_vec(),
                1,
            )
            .unwrap();
        assert_eq!(state.repository.stashes.len(), 1);
        assert!(state.queue.iter().all(|command| !matches!(
            command.purpose,
            Purpose::Repository(Query::StashVerifySelector { .. } | Query::StashDropped)
        )));
    }

    #[test]
    fn a_shifted_reflog_selector_is_retained_even_after_successful_application() {
        let mut state = State::default();
        state
            .repository_stash_finished(
                Query::StashVerifySelector {
                    stash: stash(),
                    applied: true,
                },
                "b".repeat(40).into_bytes(),
                Vec::new(),
                0,
            )
            .unwrap();
        assert!(
            state
                .queue
                .iter()
                .all(|command| command.args.get(1).is_none_or(|arg| arg != "drop"))
        );
        assert!(state.message.contains("retained") || state.message.contains("保留"));
    }

    #[test]
    fn stash_creation_rejects_a_changed_file_set_after_preview() {
        let mut state = State::default();
        assert!(
            state
                .repository_stash_finished(
                    Query::StashCreateVerify {
                        expected: b"? reviewed.txt\0".to_vec()
                    },
                    b"? new.txt\0".to_vec(),
                    Vec::new(),
                    0
                )
                .is_err()
        );
        assert!(state.queue.is_empty());
    }

    /// A text-patch failure cannot strand an otherwise valid opaque/large immutable stash.
    #[test]
    fn summary_preview_remains_actionable_when_the_text_patch_cannot_be_loaded() {
        let mut state = State::default();
        let entry = stash();
        state.repository.stash_selected = Some(entry.clone());
        state
            .repository_stash_finished(
                Query::StashPreview {
                    oid: entry.oid.clone(),
                },
                b"large.txt | 1 +\n".to_vec(),
                Vec::new(),
                0,
            )
            .unwrap();
        assert!(state.repository.stash_preview_ready);
        assert!(state.repository.stash_summary_only);
        state.queue.clear();
        state
            .repository_stash_finished(
                Query::StashPatch {
                    oid: entry.oid.clone(),
                },
                Vec::new(),
                b"patch limit".to_vec(),
                128,
            )
            .unwrap();
        assert_eq!(state.repository_selected_stash().unwrap().oid, entry.oid);
        assert_eq!(state.repository.stash_preview, "large.txt | 1 +\n");
        assert!(state.repository.stash_summary_only);
    }
}
