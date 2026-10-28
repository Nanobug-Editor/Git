//! Conflict sources come from exact index stages; a result is always the host's existing session.
use super::*;
pub(super) mod lifecycle;

impl MergeFile {
    /// Locate proposals against one exact native snapshot. Only ranges, fingerprints and immutable
    /// Git alternatives survive; the editable value and Undo remain exclusively in the host.
    pub(super) fn observe(&mut self, text: &str) -> Result<(), api::Failure> {
        self.result_len = text.len();
        self.result_eol = merge::Eol::detect(text);
        self.has_markers = merge::has_markers(text);
        let mut markers = merge::blocks(text);
        let changes = merge::plan::edits(&self.columns[1], text).ok();
        let mut blocks = Vec::new();
        self.nonconflicting.clear();
        for (key, region) in self.regions.iter().enumerate() {
            let base = &self.columns[1][region.base.clone()];
            let left = &self.columns[0][region.left.clone()];
            let right = &self.columns[2][region.right.clone()];
            let marker = markers.iter().position(|block| {
                merge::equivalent(&block.left, left) && merge::equivalent(&block.right, right)
            });
            let range = if let Some(index) = marker {
                Some(markers.remove(index).range)
            } else {
                changes
                    .as_ref()
                    .and_then(|changes| merge::plan::locate(&region.base, changes))
            };
            let ambiguous = range
                .as_ref()
                .is_none_or(|range| range.end > text.len() || range.start > range.end);
            let range = range
                .filter(|range| range.end <= text.len() && range.start <= range.end)
                .unwrap_or(0..text.len());
            if !region.conflict {
                let desired = if merge::equivalent(left, base) {
                    right
                } else {
                    left
                };
                if !ambiguous
                    && merge::equivalent(&text[range.clone()], base)
                    && !merge::equivalent(desired, base)
                {
                    self.nonconflicting.push(api::TextEdit {
                        range: api::TextRange {
                            start: range.start,
                            end: range.end,
                        },
                        text: self.result_eol.convert(desired),
                    });
                }
                continue;
            }
            blocks.push(merge::Block {
                key,
                fingerprint: merge::fingerprint(&text[range.clone()]),
                markers: merge::has_markers(&text[range.clone()]),
                range,
                left: self.result_eol.convert(left),
                right: self.result_eol.convert(right),
                base: self.result_eol.convert(base),
                ambiguous,
            });
        }
        // Unknown marker shapes cannot be hidden merely because the three-way plan differs.
        for (index, mut marker) in markers.into_iter().enumerate() {
            marker.key = self.regions.len() + index;
            marker.ambiguous = true;
            blocks.push(marker);
        }
        self.reviewed.retain(|key, hash| {
            blocks
                .iter()
                .any(|block| block.key == *key && block.fingerprint == *hash && !block.markers)
        });
        self.decisions.retain(|key, (hash, _)| {
            blocks
                .iter()
                .any(|block| block.key == *key && block.fingerprint == *hash)
        });
        if let Some((key, hash)) = self.pending_review.take() {
            if blocks
                .iter()
                .any(|block| block.key == key && block.fingerprint == hash && !block.markers)
            {
                self.reviewed.insert(key, hash);
            }
        }
        self.blocks = blocks;
        self.read_valid = !self.resolved && !self.proposals_suspended;
        Ok(())
    }
    pub(super) fn all_reviewed(&self) -> bool {
        self.read_valid
            && !self.has_markers
            && self.nonconflicting.is_empty()
            && self
                .blocks
                .iter()
                .all(|block| self.reviewed.get(&block.key) == Some(&block.fingerprint))
    }
    /// An absent stage is a deletion, including add/add and modify/delete conflicts.
    pub(super) fn has_stage(&self, stage: usize) -> bool {
        self.index_entry
            .windows(3)
            .any(|bytes| bytes == format!(" {stage}\t").as_bytes())
    }
}
impl State {
    /// Read only existing stages, bounding blobs before text decoding. Reopening retains native drafts.
    pub(super) fn open_conflict(&mut self, path: &str) -> Result<(), api::Failure> {
        if !self.conflict_lifecycle.ready() {
            return Err(failure(
                "Git 操作身份尚未确认，请刷新 / Git operation identity is not current; refresh first",
            ));
        }
        self.merge_selected = path.into();
        if let Some(file) = self.merge_files.get(path) {
            if let Some(document) = file.document.clone() {
                self.editor(
                    api::EditorOperation::ReadDocument {
                        document,
                        range: None,
                    },
                    &format!("merge-read:{path}"),
                )?;
                return self.show_window("conflicts");
            }
            if file.opaque {
                return self.show_window("conflicts");
            }
        }
        let expected = self
            .changes
            .iter()
            .find(|change| change.conflict && change.path == path)
            .map(|change| change.index_entry.clone())
            .ok_or_else(|| failure("Conflict no longer exists; refresh first"))?;
        let mut file = MergeFile {
            index_entry: expected,
            columns: vec![String::new(); 3],
            ..Default::default()
        };
        for (column, stage) in [2, 1, 3].into_iter().enumerate() {
            if file.has_stage(stage) {
                file.sources_pending += 1;
                self.queue.push_back(Command::new(
                    vec!["cat-file".into(), "-s".into(), format!(":{stage}:{path}")],
                    Purpose::MergeSize {
                        path: path.into(),
                        column,
                        stage,
                    },
                ));
            }
        }
        self.merge_files.insert(path.into(), file);
        Ok(())
    }
    /// Complete immutable sources before choosing text or explicit whole-version presentation.
    pub(super) fn conflict_source_ready(&mut self, path: &str) -> Result<(), api::Failure> {
        let file = self
            .merge_files
            .get_mut(path)
            .ok_or_else(|| failure("Conflict source retired"))?;
        file.sources_pending = file.sources_pending.saturating_sub(1);
        if file.sources_pending != 0 {
            return Ok(());
        }
        if !file.opaque {
            match merge::plan::regions(&file.columns[1], &file.columns[0], &file.columns[2]) {
                Ok(regions) if regions.iter().filter(|region| region.conflict).count() <= 64 => {
                    file.regions = regions
                }
                Ok(_) => {
                    file.opaque = true;
                    self.message="冲突超过 64 块，请采用完整版本或外部处理 / More than 64 conflict blocks; choose a complete version or resolve externally".into();
                }
                Err(error) => {
                    file.opaque = true;
                    self.message = error;
                }
            }
        }
        // Missing worktree results are handled by whole-version selection, never by creating a
        // second unsaved document or guessing that the missing side is an empty existing file.
        if file.opaque
            || self
                .changes
                .iter()
                .any(|change| change.path == path && change.worktree == 'D')
        {
            file.opaque = true;
            return self.show_window("conflicts");
        }
        self.editor(
            api::EditorOperation::OpenDocument {
                resource: api::ResourceIdentity::Local { path: path.into() },
            },
            &format!("merge-open:{path}"),
        )
    }
    /// Whole-version choices retain byte fidelity and are guarded by the original unmerged entries.
    pub(super) fn choose_conflict_version(&mut self, stage: usize) -> Result<(), api::Failure> {
        let path = self.merge_selected.clone();
        let file = self
            .merge_files
            .get(&path)
            .ok_or_else(|| failure("No conflict selected"))?;
        if !self.conflict_lifecycle.ready() || file.resolved || file.proposals_suspended {
            return Err(failure(
                "冲突建议已撤销，请刷新 / Conflict proposals are no longer current",
            ));
        }
        let expected = file.index_entry.clone();
        let command = if file.has_stage(stage) {
            let add = Command::new(["add", "--", &path], Purpose::Action);
            Command::new(
                [
                    "checkout",
                    if stage == 2 { "--ours" } else { "--theirs" },
                    "--",
                    &path,
                ],
                Purpose::Then(Box::new(add)),
            )
        } else {
            Command::new(["rm", "-f", "--", &path], Purpose::Action)
        };
        self.set_pending_review(
            Command::new(
                ["ls-files", "--stage", "-z", "--", &path],
                Purpose::Guard {
                    expected,
                    next: Box::new(command),
                },
            ),
            "conflicts",
            false,
        );
        Ok(())
    }

    /// All text proposals carry the current native revision. Ignoring a side records a decision,
    /// while ignoring both keeps the ancestor. Native Undo invalidates the resulting fingerprint.
    pub(super) fn conflict_action(&mut self, node: &str) -> Result<bool, api::Failure> {
        let path = self.merge_selected.clone();
        let Some(file) = self.merge_files.get_mut(&path) else {
            return Ok(false);
        };
        if node == "merge-show-base" {
            file.show_base = !file.show_base;
            return Ok(true);
        }
        if (node.starts_with("merge-block-")
            || matches!(
                node,
                "merge-from-base"
                    | "merge-nonconflicting"
                    | "resolved"
                    | "conflict-stage-2"
                    | "conflict-stage-3"
            ))
            && (!self.conflict_lifecycle.ready() || file.resolved || file.proposals_suspended)
        {
            return Err(failure(
                "冲突建议已撤销，请刷新 / Conflict proposals are no longer current",
            ));
        }
        let Some(document) = file.document.clone() else {
            return Ok(false);
        };
        if !file.read_valid || file.read_inflight {
            return Ok(false);
        }
        if node == "merge-nonconflicting" {
            let edits = file.nonconflicting.clone();
            if !edits.is_empty() {
                self.editor(
                    api::EditorOperation::ApplyEdits {
                        document,
                        edits,
                        selection: api::TextRange { start: 0, end: 0 },
                    },
                    "merge-nonconflicting",
                )?;
            }
            return Ok(true);
        }
        if node == "merge-from-base" {
            let range = api::TextRange {
                start: 0,
                end: file.result_len,
            };
            let text = file.result_eol.convert(&file.columns[1]);
            file.reviewed.clear();
            file.decisions.clear();
            self.editor(
                api::EditorOperation::ApplyEdits {
                    document,
                    edits: vec![api::TextEdit { range, text }],
                    selection: api::TextRange { start: 0, end: 0 },
                },
                "merge-base",
            )?;
            return Ok(true);
        }
        let Some(choice) = node.strip_prefix("merge-block-") else {
            return Ok(false);
        };
        let (key, action) = choice
            .split_once('-')
            .ok_or_else(|| failure("Invalid block action"))?;
        let key = key
            .parse::<usize>()
            .map_err(|_| failure("Invalid conflict block"))?;
        let block = file
            .blocks
            .iter()
            .find(|block| block.key == key)
            .ok_or_else(|| failure("Conflict block changed"))?;
        if action == "confirm" {
            if block.markers {
                return Err(failure(
                    "先删除该块的冲突标记 / Remove conflict markers before confirming",
                ));
            }
            file.reviewed.insert(key, block.fingerprint);
            return Ok(true);
        }
        if block.ambiguous {
            return Err(failure(
                "该块已被手动改写，请检查结果并确认 / This block was rewritten; review and confirm the manual result",
            ));
        }
        let text = match action {
            "left" => block.left.clone(),
            "right" => block.right.clone(),
            "ignore-left" | "ignore-right" => {
                file.reviewed.remove(&key);
                let decision = file
                    .decisions
                    .entry(key)
                    .or_insert((block.fingerprint, [0, 0]));
                decision.1[usize::from(action == "ignore-right")] = 2;
                if decision.1 != [2, 2] {
                    return Ok(true);
                }
                if !block.markers && block.fingerprint == merge::fingerprint(&block.base) {
                    file.reviewed.insert(key, block.fingerprint);
                    return Ok(true);
                }
                block.base.clone()
            }
            _ => return Err(failure("Unknown conflict action")),
        };
        let range = api::TextRange {
            start: block.range.start,
            end: block.range.end,
        };
        file.pending_review = Some((key, merge::fingerprint(&text)));
        self.editor(
            api::EditorOperation::ApplyEdits {
                document,
                edits: vec![api::TextEdit { range, text }],
                selection: api::TextRange {
                    start: range.start,
                    end: range.start,
                },
            },
            "merge-choice",
        )?;
        Ok(true)
    }
}
