//! NUL-separated name/status records preserve every changed path and its immutable commit state.
use std::collections::BTreeMap;

/// A path's change relative to the selected commit's parent set; mixed parent states are modified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryChange {
    /// The selected version introduces the path.
    Added,
    /// The selected version removes the path.
    Deleted,
    /// Content/type changed, or the path differs in different ways from multiple parents.
    Modified,
}

/// Decode complete `diff-tree --name-status --no-renames -m -z` output. Rename detection is off,
/// preserving both the removed and added paths; malformed/unsupported records fail as a whole.
pub(crate) fn history_changes(bytes: &[u8]) -> Result<BTreeMap<String, HistoryChange>, String> {
    let mut result = BTreeMap::new();
    if bytes.is_empty() {
        return Ok(result);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "Git path is not UTF-8")?;
    let text = text
        .strip_suffix('\0')
        .ok_or("Incomplete Git change record")?;
    let mut fields = text.split('\0');
    while let Some(status) = fields.next() {
        let path = fields
            .next()
            .filter(|path| !path.is_empty())
            .ok_or("Missing Git change path")?;
        let change = match status {
            "A" => HistoryChange::Added,
            "D" => HistoryChange::Deleted,
            "M" | "T" => HistoryChange::Modified,
            _ => return Err("Unsupported Git change status".into()),
        };
        result
            .entry(path.to_owned())
            .and_modify(|previous| {
                if *previous != change {
                    *previous = HistoryChange::Modified;
                }
            })
            .or_insert(change);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Spaces/newlines/Unicode remain exact; duplicate merge paths retain a single honest state.
    #[test]
    fn status_records_preserve_paths_and_merge_parent_union() {
        let changes = history_changes(
            b"A\0new file\0D\0gone\0M\0changed\nname\0A\0mixed\0M\0mixed\0D\0gone\0T\0kind\0",
        )
        .unwrap();
        assert_eq!(changes["new file"], HistoryChange::Added);
        assert_eq!(changes["gone"], HistoryChange::Deleted);
        assert_eq!(changes["changed\nname"], HistoryChange::Modified);
        assert_eq!(changes["mixed"], HistoryChange::Modified);
        assert_eq!(changes["kind"], HistoryChange::Modified);
        assert_eq!(changes.len(), 5);
        assert!(history_changes(b"").unwrap().is_empty());
    }
    /// Truncated/binary or unexpected rename records cannot publish a misleading partial tree.
    #[test]
    fn malformed_status_records_fail_atomically() {
        for bytes in [
            b"A\0path".as_slice(),
            b"A\0",
            b"A\0\0",
            b"R100\0old\0new\0",
            b"M\0\xff\0",
        ] {
            assert!(history_changes(bytes).is_err());
        }
    }
}
