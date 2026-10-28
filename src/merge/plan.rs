//! Bounded three-way change regions; immutable Git snapshots are the only inputs.
use std::ops::Range;

/// A line edit or newline mapping retains byte ranges in both unmodified inputs.
#[derive(Clone, Debug)]
pub(crate) struct Edit {
    pub base: Range<usize>,
    pub other: Range<usize>,
    /// EOL-only mappings shift byte positions but never create a three-way change region.
    pub eol_only: bool,
}
/// A region combines overlapping edits without treating adjacent independent changes as conflicts.
#[derive(Clone, Debug)]
pub(crate) struct Region {
    pub base: Range<usize>,
    pub left: Range<usize>,
    pub right: Range<usize>,
    pub conflict: bool,
}

/// Compare logical lines with a bounded LCS, but retain original offsets, including CRLF byte shifts.
/// Refuse an expensive comparison instead of truncating it or normalizing the caller's real text.
pub(crate) fn edits(base: &str, other: &str) -> Result<Vec<Edit>, String> {
    let a = base.split_inclusive('\n').collect::<Vec<_>>();
    let b = other.split_inclusive('\n').collect::<Vec<_>>();
    if a.len().max(b.len()) > 2000 {
        return Err("超过 2000 行的块比较限制 / Block comparison exceeds 2000 lines".into());
    }
    let a_keys = a
        .iter()
        .map(|line| super::logical_line(line))
        .collect::<Vec<_>>();
    let b_keys = b
        .iter()
        .map(|line| super::logical_line(line))
        .collect::<Vec<_>>();
    let prefix = a_keys
        .iter()
        .zip(&b_keys)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = a_keys[prefix..]
        .iter()
        .rev()
        .zip(b_keys[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let n = a.len() - prefix - suffix;
    let m = b.len() - prefix - suffix;
    if (n + 1).saturating_mul(m + 1) > 1_000_000 {
        return Err(
            "差异过大，请采用完整版本或手动处理 / Difference exceeds block comparison limit".into(),
        );
    }
    let mut cells = vec![0u16; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            cells[i * (m + 1) + j] = if a_keys[prefix + i] == b_keys[prefix + j] {
                1 + cells[(i + 1) * (m + 1) + j + 1]
            } else {
                cells[(i + 1) * (m + 1) + j].max(cells[i * (m + 1) + j + 1])
            };
        }
    }
    let offsets = |lines: &[&str]| {
        let mut result = vec![0];
        for line in lines {
            result.push(result.last().unwrap() + line.len());
        }
        result
    };
    let aa = offsets(&a);
    let bb = offsets(&b);
    let mut result = Vec::new();
    for line in 0..prefix {
        append_eol_mapping(&mut result, a[line], b[line], aa[line], bb[line]);
    }
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a_keys[prefix + i] == b_keys[prefix + j] {
            append_eol_mapping(
                &mut result,
                a[prefix + i],
                b[prefix + j],
                aa[prefix + i],
                bb[prefix + j],
            );
            i += 1;
            j += 1;
            continue;
        }
        let (start_i, start_j) = (i, j);
        while i < n || j < m {
            if i < n && j < m && a_keys[prefix + i] == b_keys[prefix + j] {
                break;
            }
            if j < m && (i == n || cells[i * (m + 1) + j + 1] > cells[(i + 1) * (m + 1) + j]) {
                j += 1;
            } else {
                i += 1;
            }
        }
        result.push(Edit {
            base: aa[prefix + start_i]..aa[prefix + i],
            other: bb[prefix + start_j]..bb[prefix + j],
            eol_only: false,
        });
    }
    for line in 0..suffix {
        let i = a.len() - suffix + line;
        let j = b.len() - suffix + line;
        append_eol_mapping(&mut result, a[i], b[j], aa[i], bb[j]);
    }
    Ok(result)
}

/// Logically matched lines may still differ in byte length. Mapping their newline separately keeps
/// a later accepted conflict or an insertion at EOF tied to its original base region.
fn append_eol_mapping(
    edits: &mut Vec<Edit>,
    base: &str,
    other: &str,
    base_start: usize,
    other_start: usize,
) {
    if base != other {
        let content_len = super::logical_line(base).0.len();
        debug_assert_eq!(super::logical_line(base), super::logical_line(other));
        edits.push(Edit {
            base: base_start + content_len..base_start + base.len(),
            other: other_start + content_len..other_start + other.len(),
            eol_only: true,
        });
    }
}
fn intersects(a: &Range<usize>, b: &Range<usize>) -> bool {
    if a.is_empty() && b.is_empty() {
        a.start == b.start
    } else if a.is_empty() {
        b.start <= a.start && a.start < b.end
    } else if b.is_empty() {
        a.start <= b.start && b.start < a.end
    } else {
        a.start < b.end && b.start < a.end
    }
}

/// Locate the complete replacement of a base region. An edit crossing another region is ambiguous;
/// callers must require manual review rather than overwrite neighboring user changes.
pub(crate) fn locate(range: &Range<usize>, changes: &[Edit]) -> Option<Range<usize>> {
    let mut before = 0isize;
    let mut inside = 0isize;
    for change in changes {
        let delta = change.other.len() as isize - change.base.len() as isize;
        if change.base.end <= range.start && !change.base.is_empty()
            || change.base.end < range.start
        {
            before += delta;
        } else if intersects(range, &change.base)
            || (range.is_empty() && change.base.start == range.start)
        {
            if change.base.start < range.start || change.base.end > range.end {
                return None;
            }
            inside += delta;
        }
    }
    Some(
        range.start.saturating_add_signed(before)..range.end.saturating_add_signed(before + inside),
    )
}

/// Regions refer back to the original snapshots; no editable result text is stored here.
pub(crate) fn regions(base: &str, left: &str, right: &str) -> Result<Vec<Region>, String> {
    let sides = [edits(base, left)?, edits(base, right)?];
    let mut all = sides
        .iter()
        .flat_map(|side| {
            side.iter()
                .filter(|edit| !edit.eol_only)
                .map(|edit| edit.base.clone())
        })
        .collect::<Vec<_>>();
    all.sort_by_key(|range| (range.start, range.end));
    let mut clusters: Vec<Range<usize>> = Vec::new();
    for range in all {
        if let Some(last) = clusters.last_mut().filter(|last| intersects(last, &range)) {
            last.end = last.end.max(range.end);
        } else {
            clusters.push(range);
        }
    }
    clusters
        .into_iter()
        .map(|base_range| {
            let a = locate(&base_range, &sides[0]).ok_or("Ambiguous left change")?;
            let b = locate(&base_range, &sides[1]).ok_or("Ambiguous right change")?;
            let original = &base[base_range.clone()];
            let conflict = !super::equivalent(&left[a.clone()], original)
                && !super::equivalent(&right[b.clone()], original)
                && !super::equivalent(&left[a.clone()], &right[b.clone()]);
            Ok(Region {
                base: base_range,
                left: a,
                right: b,
                conflict,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_changes_stay_separate_and_add_add_has_no_ancestor() {
        let changes = regions("a\nb\nc\n", "A\nb\nc\n", "a\nb\nC\n").unwrap();
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().all(|change| !change.conflict));
        let changes = regions("", "左🎉\n", "右\n").unwrap();
        assert_eq!(changes.len(), 1);
        assert!(changes[0].conflict);
        assert!(changes[0].base.is_empty());
    }
    #[test]
    fn manual_edit_crossing_regions_cannot_be_automatically_replaced() {
        let changes = edits("a\nb\nc\n", "manual\n").unwrap();
        assert!(locate(&(2..4), &changes).is_none());
        let changes = edits("a\nb\nc\n", "a\n中文😀\nc\n").unwrap();
        assert_eq!(locate(&(2..4), &changes), Some(2.."a\n中文😀\n".len()));
    }
}
