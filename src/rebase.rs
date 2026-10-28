//! Interactive plans remain plugin policy; an approved private dependency bridges Git's editor protocol.
use super::*;
use rebase_model::{Plan, Request, Step};

impl State {
    /// Parse full original messages so selecting Reword never silently discards an existing body.
    pub(super) fn prepare_rebase_plan(
        &mut self,
        base: String,
        head: String,
        bytes: &[u8],
    ) -> Result<(), api::Failure> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| failure("Rebase metadata is not UTF-8"))?;
        let fields = text.split('\0').collect::<Vec<_>>();
        let mut steps = Vec::new();
        for fields in fields.chunks(4) {
            if fields.len() < 4 {
                if fields.iter().any(|field| !field.trim().is_empty()) {
                    return Err(failure("Incomplete rebase range"));
                }
                break;
            }
            if fields[1].split_whitespace().count() > 1 {
                return Err(failure(
                    "不整理合并拓扑 / Interactive merge topology is not supported",
                ));
            }
            steps.push(Step {
                oid: fields[0].trim().into(),
                action: "pick".into(),
                title: fields[2].into(),
                message: fields[3].into(),
            });
        }
        if steps.is_empty() || steps.len() > 64 {
            return Err(failure(
                "请选择包含 1–64 个普通提交的范围 / Select 1–64 ordinary commits",
            ));
        }
        if let Some(prior) = &self.interactive_plan {
            let mut remaining = steps;
            steps = Vec::new();
            for old in &prior.steps {
                if let Some(index) = remaining.iter().position(|step| step.oid == old.oid) {
                    remaining.remove(index);
                    steps.push(old.clone());
                }
            }
            steps.extend(remaining);
        }
        let api::Value::ResolvedProgram { program: git } =
            api::guest::request(api::Operation::Process {
                operation: plugin_protocol::process::Operation::Resolve {
                    program: "git".into(),
                },
            })?
        else {
            return Err(failure("Cannot resolve Git"));
        };
        self.rebase_draft = Some(Plan {
            workspace: self.environment.workspace.clone(),
            git,
            base,
            head,
            branch: (!self.current_ref.is_empty()).then(|| self.current_ref.clone()),
            original: steps.iter().map(|step| step.oid.clone()).collect(),
            steps,
        });
        Ok(())
    }
    /// Prepare literal stdin. The dependency validates range/OIDs again before starting native Git.
    pub(super) fn rebase_command(&self, plan: &Plan, mode: &str) -> Result<Command, api::Failure> {
        let mut retained = false;
        for step in &plan.steps {
            if matches!(step.action.as_str(), "squash" | "fixup") && !retained {
                return Err(failure(
                    "首个保留提交不能合并到前一条 / Squash or fixup needs an earlier retained commit",
                ));
            }
            if matches!(step.action.as_str(), "reword" | "squash") && step.message.trim().is_empty()
            {
                return Err(failure("请输入提交说明 / Enter a commit message"));
            }
            retained |= step.action != "drop";
        }
        let bytes = serde_json::to_vec(&Request {
            mode: mode.into(),
            plan: plan.clone(),
        })
        .map_err(|error| failure(error.to_string()))?;
        if bytes.len() > 262144 {
            return Err(failure(
                "变基计划超过 256 KiB / Rebase plan exceeds 256 KiB",
            ));
        }
        let mut command = Command::new(
            vec!["rebase".into(), format!("--{mode}"), plan.base.clone()],
            Purpose::Action,
        );
        command.service = Some("rebase".into());
        command.input = Some(bytes);
        command.display = Some(format!(
            "{}\n{} → {}\n{}",
            plan.branch.as_deref().unwrap_or("detached HEAD"),
            plan.head,
            plan.base,
            plan.steps
                .iter()
                .map(|step| format!(
                    "{} {} {}{}",
                    step.action,
                    step.oid,
                    step.title,
                    if matches!(step.action.as_str(), "reword" | "squash") {
                        format!("\n{}", step.message)
                    } else {
                        String::new()
                    }
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        Ok(command)
    }
    /// Native plan controls use current scene revisions; row positions never become commit identities.
    pub(super) fn edit_rebase(&mut self, event: &ui::UiEvent) -> Result<bool, api::Failure> {
        let Some(plan) = self.rebase_draft.as_mut() else {
            return Ok(false);
        };
        if event.node == "rebase-cancel" {
            self.rebase_draft = None;
            return Ok(true);
        }
        if let Some(index) = event
            .node
            .strip_prefix("rebase-action-")
            .and_then(|value| value.parse::<usize>().ok())
        {
            if let ui::Action::Select(action) = &event.action
                && matches!(
                    action.as_str(),
                    "pick" | "reword" | "squash" | "fixup" | "drop"
                )
            {
                if index < plan.steps.len() {
                    // Squash starts with both messages. Users may then replace this explicit
                    // combined result; choosing Squash must not silently discard the earlier body.
                    if action == "squash" && plan.steps[index].action != "squash" {
                        if let Some(prior) = plan.steps[..index]
                            .iter()
                            .rev()
                            .find(|step| step.action != "drop")
                        {
                            plan.steps[index].message = format!(
                                "{}\n\n{}",
                                prior.message.trim_end(),
                                plan.steps[index].message.trim_end()
                            );
                            *self
                                .input_epochs
                                .entry(format!("rebase-message-{index}"))
                                .or_default() += 1;
                        }
                    }
                    plan.steps[index].action = action.clone();
                }
            }
            return Ok(true);
        }
        if let Some(index) = event
            .node
            .strip_prefix("rebase-message-")
            .and_then(|value| value.parse::<usize>().ok())
        {
            if let ui::Action::Change(message) = &event.action
                && let Some(step) = plan.steps.get_mut(index)
            {
                step.message = message.clone();
            }
            return Ok(true);
        }
        for (prefix, up) in [("rebase-up-", true), ("rebase-down-", false)] {
            if let Some(index) = event
                .node
                .strip_prefix(prefix)
                .and_then(|value| value.parse::<usize>().ok())
            {
                if index < plan.steps.len() {
                    let target = if up {
                        index.saturating_sub(1)
                    } else {
                        (index + 1).min(plan.steps.len() - 1)
                    };
                    plan.steps.swap(index, target);
                    for row in [index, target] {
                        *self
                            .input_epochs
                            .entry(format!("rebase-message-{row}"))
                            .or_default() += 1;
                    }
                }
                return Ok(true);
            }
        }
        if event.node == "rebase-run" {
            let plan = self.rebase_draft.clone().unwrap();
            let command = self.rebase_command(&plan, "start")?;
            if let Some(data) = &self.private_data {
                api::guest::write_file(
                    data,
                    "rebase-plan.json",
                    serde_json::to_vec(&plan).map_err(|error| failure(error.to_string()))?,
                )?;
            }
            self.interactive_plan = Some(plan);
            self.rebase_draft = None;
            self.set_pending_review(command, "history", !self.changes.is_empty());
            return Ok(true);
        }
        Ok(false)
    }
    /// Clearing a finished plan prevents a later, unrelated external rebase from adopting it.
    pub(super) fn clear_rebase_plan(&mut self) -> Result<(), api::Failure> {
        self.interactive_plan = None;
        if let Some(data) = &self.private_data {
            api::guest::write_file(data, "rebase-plan.json", b"null".to_vec())?;
        }
        Ok(())
    }
    /// A native scrollable form exposes reorder/actions/messages without an executable todo editor.
    pub(super) fn rebase_dialog(&self) -> Option<ui::Dialog> {
        let plan = self.rebase_draft.as_ref()?;
        let mut rows = Vec::new();
        for (index, step) in plan.steps.iter().enumerate() {
            let mut nodes = vec![
                ui::Node::text(
                    format!("rebase-title-{index}"),
                    format!("{} · {}", &step.oid[..8], step.title),
                ),
                ui::Node::row(
                    format!("rebase-controls-{index}"),
                    vec![
                        self.button(&format!("rebase-up-{index}"), "上移", "Up")
                            .disabled(self.ui_busy() || index == 0),
                        self.button(&format!("rebase-down-{index}"), "下移", "Down")
                            .disabled(self.ui_busy() || index + 1 == plan.steps.len()),
                        ui::Node::new(
                            format!("rebase-action-{index}"),
                            ui::Kind::Choice {
                                options: [
                                    ("pick", "保留", "Pick"),
                                    ("reword", "改说明", "Reword"),
                                    ("squash", "合并并改说明", "Squash"),
                                    ("fixup", "合并保留前条说明", "Fixup"),
                                    ("drop", "丢弃", "Drop"),
                                ]
                                .into_iter()
                                .map(|(id, zh, en)| ui::OptionItem {
                                    id: id.into(),
                                    label: self.label(zh, en).into(),
                                    disabled: false,
                                })
                                .collect(),
                                selected: Some(step.action.clone()),
                            },
                        )
                        .disabled(self.ui_busy()),
                    ],
                )
                .wrap(),
            ];
            if matches!(step.action.as_str(), "reword" | "squash") {
                nodes.push(
                    ui::Node::textarea(
                        format!("rebase-message-{index}"),
                        ui::Input {
                            value: step.message.clone(),
                            value_revision: self
                                .input_epochs
                                .get(&format!("rebase-message-{index}"))
                                .copied()
                                .unwrap_or(0),
                            placeholder: self.label("提交说明", "Commit message").into(),
                        },
                    )
                    .height(90.),
                );
            }
            rows.push(ui::Node::column(format!("rebase-row-{index}"), nodes));
        }
        Some(ui::Dialog {
            id: "rebase-plan".into(),
            title: self
                .label(
                    "整理提交 · 预览后执行",
                    "Interactive rebase · Review before execution",
                )
                .into(),
            width: 850.,
            content: ui::Node::column(
                "rebase-form",
                vec![
                    ui::Node::scroll("rebase-scroll", ui::Node::column("rebase-steps", rows))
                        .height(420.),
                    ui::Node::row(
                        "rebase-footer",
                        vec![
                            self.button("rebase-run", "核对执行计划…", "Review execution plan…"),
                            self.button("rebase-cancel", "取消", "Cancel"),
                        ],
                    ),
                ],
            ),
        })
    }
}
