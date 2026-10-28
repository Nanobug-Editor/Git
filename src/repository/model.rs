//! Pure repository identities, pull choices and byte-delimited stash records.

/// A single worktree identity does not grant direct access to its possibly external Git directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    pub root: String,
    pub git_dir: String,
    pub common_dir: String,
}
impl Identity {
    /// `--show-prefix` must be empty: all other guest paths are workspace-relative Git paths.
    /// A linked worktree remains valid even when both metadata directories are outside the root.
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        let text = std::str::from_utf8(bytes).map_err(|_| "Repository paths are not UTF-8")?;
        let fields = text.lines().collect::<Vec<_>>();
        if fields.len() != 5 || fields[0] != "false" {
            return Err("Open a non-bare Git working tree");
        }
        if !fields[1].is_empty() {
            return Err("Open the repository root, rather than one of its subdirectories");
        }
        if fields[2..]
            .iter()
            .any(|value| value.is_empty() || value.contains('\0'))
        {
            return Err("Incomplete repository identity");
        }
        Ok(Self {
            root: fields[2].into(),
            git_dir: fields[3].into(),
            common_dir: fields[4].into(),
        })
    }

    /// Stable names fit the public preference-key grammar without exposing paths as key syntax.
    pub fn preference_name(&self) -> String {
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in self.root.bytes().chain([0]).chain(self.git_dir.bytes()) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
        format!("repository-{hash:016x}")
    }

    pub fn linked(&self) -> bool {
        self.git_dir.replace('\\', "/") != self.common_dir.replace('\\', "/")
    }
}

/// A remembered choice is explicit; absence always means fast-forward only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PullStrategy {
    Merge,
    Rebase,
}
impl PullStrategy {
    pub fn name(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "merge" => Some(Self::Merge),
            "rebase" => Some(Self::Rebase),
            _ => None,
        }
    }
}

/// The immutable endpoints behind a divergence decision; later queries cannot silently replace them.
#[derive(Clone, Debug)]
pub(crate) struct Divergence {
    pub head: String,
    pub upstream: String,
    pub ahead: u64,
    pub behind: u64,
}

/// An OID owns the preview. A mutable reflog selector is used only after a separate identity check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stash {
    pub selector: String,
    pub oid: String,
    pub title: String,
    pub time: String,
}

pub(super) fn oid(bytes: &[u8]) -> Result<String, &'static str> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| "Invalid Git identity")?
        .trim();
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Expected a complete Git object identity");
    }
    Ok(value.into())
}

/// Four NUL fields per stash preserve spaces and line breaks in the stash subject.
pub(super) fn stashes(bytes: &[u8]) -> Result<Vec<Stash>, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Stash metadata is not UTF-8")?;
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut fields = text.split('\0').collect::<Vec<_>>();
    if fields.last() == Some(&"") {
        fields.pop();
    }
    if !fields.len().is_multiple_of(4) {
        return Err("Incomplete stash list");
    }
    fields
        .chunks(4)
        .map(|fields| {
            let selector = fields[0].trim();
            let index = selector
                .strip_prefix("stash@{")
                .and_then(|value| value.strip_suffix('}'));
            if !index.is_some_and(|index| {
                !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
            }) {
                return Err("Invalid stash selector");
            }
            Ok(Stash {
                selector: selector.into(),
                oid: oid(fields[1].as_bytes())?,
                title: fields[2].into(),
                time: fields[3].into(),
            })
        })
        .collect()
}

/// Both sides must contain unique commits before a failed ff-only pull is called a divergence.
pub(super) fn divergence(bytes: &[u8]) -> Result<(u64, u64), &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Invalid divergence count")?;
    let counts = text
        .split_whitespace()
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Invalid divergence count")?;
    if counts.len() != 2 {
        return Err("Incomplete divergence count");
    }
    Ok((counts[0], counts[1]))
}

/// Truncation is only a visible read-only preview policy; no write is constructed from this text.
pub(super) fn preview(bytes: &[u8], limit: usize) -> (String, bool) {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return (
            "非 UTF-8 输出：保留原始对象，不转换文本 / Non UTF-8 output; original objects retained"
                .into(),
            false,
        ),
    };
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].into(), end < text.len())
}

/// UTC is explicit because the public environment carries no user time-zone offset.
pub(super) fn utc(seconds: u64) -> String {
    let seconds = seconds.min(253_402_300_799);
    let days = (seconds / 86_400) as i64;
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let clock = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        clock / 3600,
        clock / 60 % 60,
        clock % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_discovery_supports_linked_worktrees_but_rejects_nested_projects() {
        let identity =
            Identity::parse(b"false\n\nC:/project\nD:/main/.git/worktrees/project\nD:/main/.git\n")
                .unwrap();
        assert!(identity.linked());
        assert!(
            Identity::parse(b"false\nsrc/\nC:/project\nC:/project/.git\nC:/project/.git\n")
                .is_err()
        );
        assert!(Identity::parse(b"true\n\n\n\n\n").is_err());
    }

    #[test]
    fn stash_identity_is_not_derived_from_a_mutable_list_position() {
        let output = format!(
            "stash@{{3}}\0{}\0WIP: spaces 中\nbody\0date\0",
            "a".repeat(40)
        );
        let parsed = stashes(output.as_bytes()).unwrap();
        assert_eq!(parsed[0].selector, "stash@{3}");
        assert_eq!(parsed[0].oid, "a".repeat(40));
        assert_eq!(parsed[0].title, "WIP: spaces 中\nbody");
        assert!(stashes(b"stash@{0}\0short\0title\0date\0").is_err());
        assert!(stashes(b"stash@{0}\0").is_err());
    }

    #[test]
    fn divergence_and_preview_do_not_confuse_authentication_or_partial_text() {
        assert_eq!(divergence(b"2\t3\n").unwrap(), (2, 3));
        assert_eq!(divergence(b"0 3").unwrap(), (0, 3));
        assert!(divergence(b"fatal: auth failed").is_err());
        assert_eq!(preview("中😀test".as_bytes(), 4), ("中".into(), true));
    }

    #[test]
    fn timestamps_are_explicit_and_calendar_correct() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc(u64::MAX), "9999-12-31 23:59:59 UTC");
    }
}
