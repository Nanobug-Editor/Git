//! IDEA-style grouping is generated once for both the title popup and branch-tree context menus.
use super::*;
use ui::{ActionMenuItem as Item, ActionMenuKind as Kind};

impl State {
    /// Use existing applicability policy while changing the public menu's grouping and target labels.
    pub(crate) fn branch_actions(&self, reference: &str) -> Vec<Item> {
        self.branch_actions_inner(reference, true)
    }
    /// A single tracking hop prevents custom local-upstream cycles from expanding recursively.
    fn branch_actions_inner(&self, reference: &str, include_tracking: bool) -> Vec<Item> {
        let current = !reference.is_empty() && reference == self.current_ref;
        let local =
            reference.starts_with("refs/heads/") && self.refs.iter().any(|item| item == reference);
        let known = self.refs.iter().any(|item| item == reference);
        let available = self.branch_menu(reference);
        let get = |action: &str, en: String, zh: String| {
            available
                .iter()
                .find(|item| item.arguments["action"] == action)
                .map(|command| {
                    let mut command = command.clone();
                    command.label = ui::LocalizedText::new(en, zh);
                    command.separator_before = false;
                    Item::action(format!("action-{action}"), command)
                })
        };
        let name = branch_name(reference);
        let head = branch_name(&self.current_ref);
        let mut items = Vec::new();
        if local && !current {
            if let Some(item) = get("switch", "Checkout".into(), "签出".into()) {
                items.push(item);
            }
        }
        if known && reference.starts_with("refs/remotes/") {
            if let Some(item) = get(
                "checkout-remote",
                "Checkout as a local tracking branch…".into(),
                "签出为本地跟踪分支…".into(),
            ) {
                items.push(item);
            }
        }
        if known {
            if let Some(item) = get(
                "branch",
                format!("New branch from '{name}'…"),
                format!("从 '{name}' 新建分支…"),
            ) {
                items.push(item);
            }
        }
        // HEAD is queried after refs. Keep this command's identity stable while that read is
        // pending; cached tree updates may change availability but cannot add command intent.
        if known && !current {
            let mut compare = self.branch_comparison_command(
                "compare-current",
                reference,
                format!("Compare with '{head}'"),
                format!("与 '{head}' 比较"),
            );
            compare.separator_before = true;
            items.push(compare);
        }
        if known {
            items.push(self.branch_comparison_command(
                "compare-worktree",
                reference,
                "Show diff with working tree".into(),
                "显示与工作树的差异".into(),
            ));
        }
        if known && !current {
            if let Some(mut item) = get(
                "rebase",
                format!("Rebase '{head}' onto '{name}'…"),
                format!("将 '{head}' 变基到 '{name}'…"),
            ) {
                item.separator_before = true;
                items.push(item);
            }
            if let Some(item) = get(
                "merge",
                format!("Merge '{name}' into '{head}'…"),
                format!("将 '{name}' 合并到 '{head}'…"),
            ) {
                items.push(item);
            }
        }
        if known {
            if let Some(mut item) = get("pull", "Update…".into(), "更新…".into()) {
                item.separator_before = true;
                items.push(item);
            }
            if let Some(item) = get("push", "Push…".into(), "推送…".into()) {
                items.push(item);
            }
        }
        if let Some(upstream) = self
            .ref_upstreams
            .get(reference)
            .filter(|upstream| include_tracking && !upstream.is_empty())
        {
            // The upstream is metadata from the same ref observation. Opening it never fetches.
            let children = if upstream != reference {
                self.branch_actions_inner(upstream, false)
            } else {
                Vec::new()
            };
            items.push(submenu(
                "tracked",
                ui::LocalizedText::new(
                    format!("Tracked branch '{}'", branch_name(upstream)),
                    format!("跟踪的分支 '{}'", branch_name(upstream)),
                ),
                children,
            ));
        }
        if local {
            if let Some(mut item) = get("branch-rename", "Rename…".into(), "重命名…".into())
            {
                item.separator_before = true;
                items.push(item);
            }
            if !current {
                if let Some(item) = get("branch-delete", "Delete…".into(), "删除…".into()) {
                    items.push(item);
                }
            }
        }
        let used = [
            "switch",
            "checkout-remote",
            "branch",
            "merge",
            "rebase",
            "pull",
            "push",
            "branch-rename",
            "branch-delete",
        ];
        let extras = available
            .into_iter()
            .filter(|item| !used.contains(&item.arguments["action"].as_str().unwrap_or_default()))
            .map(|command| {
                Item::action(
                    format!(
                        "extra-{}",
                        command.arguments["action"].as_str().unwrap_or_default()
                    ),
                    command,
                )
            })
            .collect::<Vec<_>>();
        if !extras.is_empty() {
            let mut more = submenu(
                "more-actions",
                ui::LocalizedText::new("More operations", "更多操作"),
                extras,
            );
            more.separator_before = true;
            items.push(more);
        }
        // A nested tracked ref gets its own namespace; no duplicate row identity is admitted.
        namespace(&mut items, &format!("{}-", trees::node_id(reference)));
        items
    }
    /// Top-level commands are concise; all branch leaves open the shared contextual action model.
    pub(super) fn workspace_menu(&self) -> Vec<Item> {
        let commands = self.branch_menu(&self.current_ref);
        let mut items = Vec::new();
        for (action, en, zh) in [
            ("pull", "Update project…", "更新项目…"),
            ("push", "Push…", "推送…"),
        ] {
            if let Some(command) = commands
                .iter()
                .find(|command| command.arguments["action"] == action)
            {
                let mut command = command.clone();
                command.label = ui::LocalizedText::new(en, zh);
                command.separator_before = false;
                items.push(Item::action(format!("root-{action}"), command));
            }
        }
        items.insert(
            1.min(items.len()),
            Item::action(
                "root-commit",
                self.workspace_command("commit", "", "Commit…", "提交…"),
            ),
        );
        if let Some(command) = commands
            .iter()
            .find(|command| command.arguments["action"] == "branch")
        {
            let mut item = Item::action("root-branch", command.clone());
            item.separator_before = true;
            items.push(item);
        }
        if self.refs.contains(&self.current_ref) {
            items.push(group(
                "head",
                "HEAD (current branch)",
                "HEAD（当前分支）",
                true,
                vec![self.branch_row(&self.current_ref, "head")],
            ));
        }
        let recent = self
            .workspace_ui
            .recent
            .iter()
            .filter(|reference| self.refs.contains(reference))
            .map(|reference| self.branch_row(reference, "recent"))
            .collect();
        items.push(group("recent", "Recent", "最近", true, recent));
        for (prefix, id, en, zh) in [
            ("refs/heads/", "local", "Local", "本地"),
            ("refs/remotes/", "remote", "Remote", "远程"),
        ] {
            let mut directory = model::Directory::default();
            for reference in self
                .refs
                .iter()
                .filter_map(|reference| reference.strip_prefix(prefix))
            {
                directory.insert(reference);
            }
            items.push(group(
                id,
                en,
                zh,
                false,
                self.branch_directories(&directory, prefix, id, 0),
            ));
        }
        items
    }
    /// Preserve native directory grouping while branch labels retain enough path for global search.
    fn branch_directories(
        &self,
        directory: &model::Directory,
        prefix: &str,
        scope: &str,
        depth: usize,
    ) -> Vec<Item> {
        // Very deep ref paths keep their complete searchable names without exceeding the public
        // native menu's nesting budget once branch actions and tracking submenus are included.
        if depth >= 2 {
            fn leaves<'a>(directory: &'a model::Directory, out: &mut Vec<&'a str>) {
                out.extend(directory.files.iter().map(|(_, path)| path.as_str()));
                for child in directory.children.values() {
                    leaves(child, out);
                }
            }
            let mut paths = Vec::new();
            leaves(directory, &mut paths);
            return paths
                .into_iter()
                .map(|path| {
                    self.branch_row(
                        &format!(
                            "{}{path}",
                            if scope == "local" {
                                "refs/heads/"
                            } else {
                                "refs/remotes/"
                            }
                        ),
                        scope,
                    )
                })
                .collect();
        }
        let mut items = Vec::new();
        for (name, child) in &directory.children {
            let next = format!("{prefix}{name}/");
            let id = format!("{scope}-{}", trees::node_id(&next));
            let mut row = group(
                &id,
                name,
                name,
                false,
                self.branch_directories(child, &next, scope, depth + 1),
            );
            row.icon = Some(ui::ToolIcon::new(
                "icons/folder.svg",
                "icons/folder-dark.svg",
            ));
            items.push(row);
        }
        for (_, path) in &directory.files {
            let reference = if scope == "local" {
                format!("refs/heads/{path}")
            } else {
                format!("refs/remotes/{path}")
            };
            items.push(self.branch_row(&reference, scope));
        }
        items
    }
    /// Lazy preparation materializes only the selected branch's action list, independent of history.
    fn branch_row(&self, reference: &str, scope: &str) -> Item {
        let name = branch_name(reference);
        let mut row = submenu(
            &format!("{scope}-{}", trees::node_id(reference)),
            ui::LocalizedText::new(name, name),
            if self.workspace_ui.opened_branch.as_deref() == Some(reference) {
                self.branch_actions(reference)
            } else {
                Vec::new()
            },
        );
        row.checked = reference == self.current_ref;
        row.icon = Some(ui::ToolIcon::new("icons/git.svg", "icons/git.svg"));
        row.detail = self
            .ref_upstreams
            .get(reference)
            .filter(|value| !value.is_empty())
            .map(|value| ui::LocalizedText::new(branch_name(value), branch_name(value)));
        if let Kind::Submenu { on_open, .. } = &mut row.kind {
            *on_open = Some(self.workspace_command(
                "branch-menu",
                reference,
                "Branch actions",
                "分支操作",
            ));
        }
        namespace_children(&mut row, &format!("{scope}-"));
        row
    }
    /// Comparison metadata stays on the same typed branch command boundary as mutations.
    fn branch_comparison_command(
        &self,
        action: &str,
        reference: &str,
        en: String,
        zh: String,
    ) -> Item {
        let disabled = self.ui_busy()
            || (action == "compare-current" && self.head.is_none())
            || self.pending.is_some()
            || self.branch_prompt.is_some()
            || self.workspace_ui.comparison.is_some();
        Item::action(
            format!("action-{action}"),
            tree::Command {
                command: "branch-context".into(),
                label: ui::LocalizedText::new(en, zh),
                arguments: serde_json::json!({"action":action,"target":reference}),
                disabled,
                separator_before: false,
            },
        )
    }
}

/// Directory groups do not execute an action and start with provider-chosen expansion once.
fn group(id: &str, en: &str, zh: &str, expanded: bool, items: Vec<Item>) -> Item {
    Item {
        id: id.into(),
        label: ui::LocalizedText::new(en, zh),
        detail: None,
        icon: None,
        checked: false,
        disabled: false,
        separator_before: false,
        kind: Kind::Group { expanded, items },
    }
}
/// Side submenus share the native menu model with contextual tree actions.
fn submenu(id: &str, label: ui::LocalizedText, items: Vec<Item>) -> Item {
    Item {
        id: id.into(),
        label,
        detail: None,
        icon: None,
        checked: false,
        disabled: false,
        separator_before: false,
        kind: Kind::Submenu {
            items,
            on_open: None,
        },
    }
}
/// Stable prefixes distinguish repeated recent/local references and nested tracking menus.
fn namespace(items: &mut [Item], prefix: &str) {
    for item in items {
        item.id = format!("{prefix}{}", item.id);
        namespace_children(item, prefix);
    }
}
/// Apply a namespace to descendants while retaining the branch row's own stable lookup identity.
fn namespace_children(item: &mut Item, prefix: &str) {
    match &mut item.kind {
        Kind::Group { items, .. } | Kind::Submenu { items, .. } => namespace(items, prefix),
        _ => {}
    }
}
