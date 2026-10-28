//! User-owned topology colors use the public configuration hook, never repository files or snapshots.
use super::*;
use plugin_protocol::settings::{Effective, Phase, Proposal, Source};

/// Separate normalized RGB palettes keep ordinary scroll frames free of settings parsing/allocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    /// Eight user-wide light-theme slots in the graph's stable strand order.
    light: [u32; graph::COLOR_COUNT],
    /// Independent dark-theme slots; editing one theme cannot overwrite the other.
    dark: [u32; graph::COLOR_COUNT],
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            light: Self::defaults(false),
            dark: Self::defaults(true),
        }
    }
}

impl Palette {
    /// Return the original built-in colors for the requested theme, also used by manifest defaults.
    pub(super) fn defaults(dark: bool) -> [u32; graph::COLOR_COUNT] {
        if dark {
            [
                0x70a5ff, 0xffb05c, 0x67cd87, 0xbd95ff, 0xff89bb, 0x5fd0d5, 0xdac65d, 0xff8c81,
            ]
        } else {
            [
                0x2563eb, 0xc05a00, 0x16833d, 0x8b42c9, 0xca3478, 0x07858d, 0x8b7000, 0xd64539,
            ]
        }
    }

    /// Copy only the active eight RGB values; cache keys compare actual colors rather than theme names.
    pub(super) fn colors(&self, dark: bool) -> [u32; graph::COLOR_COUNT] {
        if dark { self.dark } else { self.light }
    }

    /// Parse a complete host-resolved map atomically; absent keys retain defaults for older profiles.
    /// Explicit malformed colors or project overrides return field errors before any state changes.
    fn parse(values: &Effective) -> Result<Self, Proposal> {
        let mut palette = Self::default();
        let mut errors = Proposal::default();
        for dark in [false, true] {
            for index in 0..graph::COLOR_COUNT {
                let key = format!(
                    "graph.colors.{}.{}",
                    if dark { "dark" } else { "light" },
                    index + 1
                );
                let Some(setting) = values.get(&key) else {
                    continue;
                };
                let rgb = setting.value.as_str().and_then(|text| {
                    let digits = text.strip_prefix('#')?;
                    (digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
                        .then(|| u32::from_str_radix(digits, 16).ok())
                        .flatten()
                });
                if setting.source == Source::Project || rgb.is_none() {
                    errors.errors.insert(key, "请输入 #RRGGBB 颜色，仅允许用户设置 / Enter a #RRGGBB color in user settings".into());
                } else if dark {
                    palette.dark[index] = rgb.unwrap();
                } else {
                    palette.light[index] = rgb.unwrap();
                }
            }
        }
        if errors.errors.is_empty() {
            Ok(palette)
        } else {
            Err(errors)
        }
    }
}

impl State {
    /// Configuration validation is pure. Apply/Changed replace normalized colors only after the
    /// whole map is valid; a hot change refreshes existing history without pumping/canceling Git.
    pub(crate) fn configure_palette(
        &mut self,
        phase: Phase,
        values: &Effective,
    ) -> Result<api::Output, api::Failure> {
        let parsed = Palette::parse(values);
        if phase == Phase::Validate {
            return Ok(api::Output {
                configuration: Some(parsed.err().unwrap_or_default()),
                ..Default::default()
            });
        }
        let palette = parsed.map_err(|proposal| {
            api::Failure::new(
                api::ErrorCode::InvalidRequest,
                proposal
                    .errors
                    .into_iter()
                    .map(|(key, error)| format!("{key}: {error}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
        let changed = self.history_palette.colors(self.environment.dark)
            != palette.colors(self.environment.dark);
        self.history_palette = palette;
        // Restored panels may precede their visibility receipt. Refresh the already published
        // scene rather than gating correctness on that notification; this never opens a panel.
        let views =
            if phase == Phase::Changed && changed && self.published_views.contains_key("history") {
                let views = self.history_scroll_view();
                self.changed_views(views)
            } else {
                Vec::new()
            };
        Ok(api::Output {
            views,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests;
