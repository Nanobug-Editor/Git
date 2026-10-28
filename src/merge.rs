//! Marker choices are exact-revision edit proposals; the host alone owns mutable result text and Undo.
use std::ops::Range;
pub(crate) mod plan;
#[cfg(test)]
mod tests;

/// New proposals follow the current result's dominant newline style, without changing Git snapshots.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Eol {
    #[default]
    Lf,
    Crlf,
}

impl Eol {
    /// Inspect complete line endings. A mixed result uses its majority style, then its first style.
    pub(crate) fn detect(text: &str) -> Self {
        let mut counts = [0usize; 2];
        let mut first = None;
        for line in text
            .split_inclusive('\n')
            .filter(|line| line.ends_with('\n'))
        {
            let style = if line.ends_with("\r\n") {
                Self::Crlf
            } else {
                Self::Lf
            };
            first.get_or_insert(style);
            counts[usize::from(style == Self::Crlf)] += 1;
        }
        match counts[0].cmp(&counts[1]) {
            std::cmp::Ordering::Greater => Self::Lf,
            std::cmp::Ordering::Less => Self::Crlf,
            std::cmp::Ordering::Equal => first.unwrap_or_default(),
        }
    }

    /// Convert only a replacement proposal; preserve missing final newlines and literal lone CRs.
    pub(crate) fn convert(self, text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let (content, terminated) = logical_line(line);
            result.push_str(content);
            if terminated {
                result.push_str(match self {
                    Self::Lf => "\n",
                    Self::Crlf => "\r\n",
                });
            }
        }
        result
    }
}

/// Compare logical lines while preserving the distinction between a final newline and no newline.
pub(crate) fn equivalent(left: &str, right: &str) -> bool {
    left.split_inclusive('\n')
        .map(logical_line)
        .eq(right.split_inclusive('\n').map(logical_line))
}

/// A CR is formatting only when immediately followed by LF; all other source bytes remain content.
fn logical_line(line: &str) -> (&str, bool) {
    match line.strip_suffix('\n') {
        Some(content) => (content.strip_suffix('\r').unwrap_or(content), true),
        None => (line, false),
    }
}

/// One complete marker block, retaining original newline bytes and both exact source alternatives.
pub(crate) struct Block {
    pub key: usize,
    pub range: Range<usize>,
    pub left: String,
    pub right: String,
    pub base: String,
    pub markers: bool,
    pub ambiguous: bool,
    pub fingerprint: u64,
}

/// Recognize complete ordinary/diff3 Git conflict markers. Malformed/partial markers never yield edits.
pub(crate) fn blocks(text: &str) -> Vec<Block> {
    let mut result = Vec::new();
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in &lines {
        starts.push(offset);
        offset += line.len();
    }
    starts.push(offset);
    let mut i = 0;
    while i < lines.len() {
        let Some(width) = marker_width(lines[i], '<') else {
            i += 1;
            continue;
        };
        let start = i;
        i += 1;
        let left_start = i;
        while i < lines.len()
            && !is_marker(lines[i], '=', width)
            && !is_marker(lines[i], '|', width)
        {
            i += 1;
        }
        let left_end = i;
        if i < lines.len() && is_marker(lines[i], '|', width) {
            while i < lines.len() && !is_marker(lines[i], '=', width) {
                i += 1;
            }
        }
        if i == lines.len() {
            break;
        }
        i += 1;
        let right_start = i;
        while i < lines.len() && !is_marker(lines[i], '>', width) {
            i += 1;
        }
        if i == lines.len() {
            break;
        }
        result.push(Block {
            key: result.len(),
            range: starts[start]..starts[i + 1],
            left: text[starts[left_start]..starts[left_end]].into(),
            right: text[starts[right_start]..starts[i]].into(),
            base: String::new(),
            markers: true,
            ambiguous: false,
            fingerprint: fingerprint(&text[starts[start]..starts[i + 1]]),
        });
        i += 1;
    }
    result
}

/// Stable review fingerprints invalidate decisions after native editing or Undo, not after a repaint.
pub(crate) fn fingerprint(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    })
}

/// Git permits conflict-marker-size attributes. Width belongs to each opener, not a global 7-byte constant.
fn marker_width(line: &str, marker: char) -> Option<usize> {
    let width = line.chars().take_while(|ch| *ch == marker).count();
    (width >= 7 && line[width..].chars().next().is_none_or(char::is_whitespace)).then_some(width)
}
fn is_marker(line: &str, marker: char, width: usize) -> bool {
    marker_width(line, marker) == Some(width)
}

/// A partially edited, unsupported or malformed marker must not masquerade as an already solved file.
pub(crate) fn has_markers(text: &str) -> bool {
    text.lines().any(|line| {
        // A standalone separator is valid source text (for example a Markdown Setext heading).
        // Openers/closers/base markers remain conservative guards even after a partial manual edit.
        ['<', '>', '|']
            .into_iter()
            .any(|marker| marker_width(line, marker).is_some())
    })
}
