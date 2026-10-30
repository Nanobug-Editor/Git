//! Workspace-local layout preferences are plugin policy; the host stores opaque bounded values.
use super::*;
use serde::{Deserialize, Serialize};

/// Versioned durable intent excludes processes, document identities, scroll and pointer capture.
#[derive(Default, Serialize, Deserialize)]
pub(super) struct Saved {
    /// Only schema one is understood; future records are preserved without overwriting them.
    schema: u32,
    /// Stable container IDs map to normalized child weights, independent of window pixels.
    #[serde(default)]
    splits: BTreeMap<String, Vec<f32>>,
    /// Existing compact label/author/time column intent, in logical pixels.
    #[serde(default)]
    columns: Option<[f32; 3]>,
}

/// Revision is the host CAS token; uninitialized/native test states never write storage implicitly.
#[derive(Default)]
pub(super) struct Preferences {
    /// Decoded plugin-owned schema, never host layout or native resource handles.
    saved: Saved,
    /// Last persisted revision is required by the host's compare-and-set write.
    revision: u64,
    /// Failed/missing initialization cannot silently replace unreadable records during painting.
    loaded: bool,
}

/// Empty file type scopes this purpose to the owning plugin's current workspace.
fn key() -> api::PreferenceKey {
    api::PreferenceKey {
        file_type: String::new(),
        name: "window-layout".into(),
    }
}

impl Preferences {
    /// Validate persisted input before any native allocation; unsupported/corrupt data is preserved.
    fn read(&mut self, value: api::PreferenceValue) -> Result<(), api::Failure> {
        let saved: Saved = match value.data {
            Some(data) => serde_json::from_value(data)
                .map_err(|_| failure("Invalid Git layout preferences"))?,
            None => Saved {
                schema: 1,
                ..Default::default()
            },
        };
        if saved.schema != 1
            || saved.splits.len() > 32
            || saved
                .splits
                .iter()
                .any(|(id, weights)| id.len() > 128 || !valid_weights(weights))
            || saved.columns.is_some_and(|values| {
                values
                    .iter()
                    .any(|v| !v.is_finite() || !(1. ..=100_000.).contains(v))
            })
        {
            return Err(failure("Unsupported or invalid Git layout preferences"));
        }
        self.saved = saved;
        self.revision = value.revision;
        self.loaded = true;
        Ok(())
    }

    /// Restore only containers still declared by this version, including dialog descendants.
    pub(super) fn decorate(&self, document: &mut ui::Document) {
        let mut apply = |node: &mut ui::Node| {
            if node.layout.resizable {
                let count = match &node.kind {
                    ui::Kind::Row { children } | ui::Kind::Column { children } => children.len(),
                    _ => 0,
                };
                node.layout.split_proportions = Some(
                    self.saved
                        .splits
                        .get(&node.id)
                        .filter(|weights| weights.len() == count)
                        .cloned()
                        .unwrap_or_default(),
                );
            }
            Ok(())
        };
        let _ = document.root.try_visit_mut(&mut apply);
        if let Some(dialog) = &mut document.dialog {
            let _ = dialog.content.try_visit_mut(&mut apply);
        }
    }

    /// One release is one CAS write. A concurrent winner is preserved and the failure is visible.
    fn write(&mut self) -> Result<(), api::Failure> {
        if !self.loaded {
            return Ok(());
        }
        let data = serde_json::to_value(&self.saved)
            .map_err(|_| failure("Invalid Git layout preferences"))?;
        let value = api::guest::write_preference(key(), self.revision, data)?;
        self.revision = value.revision;
        Ok(())
    }
}

/// Normalized weights are portable across viewport sizes and never encode native pixel bounds.
fn valid_weights(weights: &[f32]) -> bool {
    (2..=16).contains(&weights.len())
        && weights.iter().all(|v| v.is_finite() && *v > 0. && *v <= 1.)
        && (weights.iter().sum::<f32>() - 1.).abs() <= 0.001
}

impl State {
    /// Read the durable record once before activation publishes any native workbench views.
    pub(super) fn load_layout_preferences(&mut self) -> Result<(), api::Failure> {
        if !self.layout_preferences.loaded {
            self.layout_preferences
                .read(api::guest::read_preference(key(), false)?.value)?;
            if let Some(columns) = self.layout_preferences.saved.columns {
                self.history_columns
                    .restore(Some(&serde_json::json!(columns)));
            }
        }
        Ok(())
    }

    /// Native split release affects presentation only, never cancels Git queries or changes selection.
    pub(super) fn layout_split_event(
        &mut self,
        panel: Option<&str>,
        event: &ui::UiEvent,
    ) -> Result<bool, api::Failure> {
        let ui::Action::SplitResized(weights) = &event.action else {
            return Ok(false);
        };
        let node = panel
            .and_then(|panel| self.published_views.get(panel))
            .and_then(|view| view.active_node(&event.node));
        let valid = node.is_some_and(|node| match &node.kind {
            ui::Kind::Row { children } | ui::Kind::Column { children } => {
                node.layout.resizable && weights.len() == children.len()
            }
            _ => false,
        });
        if valid && valid_weights(weights) {
            if self.layout_preferences.saved.splits.len() >= 32
                && !self
                    .layout_preferences
                    .saved
                    .splits
                    .contains_key(&event.node)
            {
                return Err(failure("Git layout preference limit exceeded"));
            }
            self.layout_preferences
                .saved
                .splits
                .insert(event.node.clone(), weights.clone());
            self.layout_preferences.write()?;
        }
        Ok(true)
    }

    /// Column intent remains in logical pixels, preserving the existing compact trailing-column policy.
    pub(super) fn save_layout_columns(&mut self) {
        if self.history_columns.trailing.is_some_and(|values| {
            values
                .iter()
                .any(|v| !v.is_finite() || !(1. ..=100_000.).contains(v))
        }) {
            return;
        }
        self.layout_preferences.saved.columns = self.history_columns.trailing;
        if let Err(error) = self.layout_preferences.write() {
            self.history_error = format!("布局保存失败 / Layout could not be saved: {error}");
        }
    }
}

#[cfg(test)]
mod tests;
