//! One bounded Git batch returns complete immutable sides without per-side process startup.
use super::*;

/// Parse one exact response per expression; only complete UTF-8 blobs or confirmed absent sides
/// are admitted. Known expressions disambiguate missing filenames containing newlines/spaces.
fn decode<'a>(
    mut bytes: &'a [u8],
    expressions: &[String],
) -> Result<Vec<Option<&'a str>>, api::Failure> {
    let mut sides = Vec::with_capacity(expressions.len());
    for expression in expressions {
        let missing = format!("{expression} missing\n");
        if let Some(rest) = bytes.strip_prefix(missing.as_bytes()) {
            sides.push(None);
            bytes = rest;
            continue;
        }
        let end = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| failure("Incomplete Git batch header"))?;
        let header =
            std::str::from_utf8(&bytes[..end]).map_err(|_| failure("Invalid Git batch header"))?;
        let fields: Vec<_> = header.split(' ').collect();
        if fields.len() != 3
            || !matches!(fields[0].len(), 40 | 64)
            || !fields[0].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(failure("Invalid Git batch object"));
        }
        let size = fields[2]
            .parse::<usize>()
            .map_err(|_| failure("Invalid Git blob size"))?;
        let body = &bytes[end + 1..];
        if size >= body.len() || body[size] != b'\n' {
            return Err(failure("Incomplete Git batch content"));
        }
        match fields[1] {
            "tree" => sides.push(None), // A directory replacing a file is an absent file side.
            "blob" if size <= 256 * 1024 && !body[..size].contains(&0) => {
                sides.push(Some(std::str::from_utf8(&body[..size]).map_err(|_| {
                    failure("二进制或非 UTF-8：不进行文本比较 / Binary or non UTF-8")
                })?));
            }
            "blob" => {
                return Err(failure(
                    "二进制或文本过大 / Binary or text comparison limit",
                ));
            }
            _ => {
                return Err(failure(
                    "子模块不是文件文本，无法比较 / A submodule is not file text and cannot be compared",
                ));
            }
        }
        bytes = &body[size + 1..];
    }
    if !bytes.is_empty() {
        return Err(failure("Unexpected trailing Git batch output"));
    }
    Ok(sides)
}

impl State {
    /// Validate the whole batch before touching cache/visible content, so a bad later side cannot
    /// publish a partial comparison or poison a subsequent immutable cache hit.
    pub(crate) fn finish_history_batch(
        &mut self,
        columns: &[usize],
        path: &str,
        output: &[u8],
    ) -> Result<(), api::Failure> {
        let expressions = columns
            .iter()
            .map(|column| {
                self.snapshot_targets
                    .get(*column)
                    .map(|oid| format!("{oid}:{path}"))
                    .ok_or_else(|| failure("Historical comparison expired"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let sides = decode(output, &expressions)?;
        // Selecting another log row may clear legacy detail columns while this immutable
        // comparison is still completing; its captured source count remains authoritative.
        self.columns
            .resize(self.snapshot_targets.len(), String::new());
        if self.diff_history {
            self.diff_columns
                .resize(self.snapshot_targets.len(), String::new());
        }
        for (column, text) in columns.iter().copied().zip(sides) {
            self.history_sources.remember(
                &self.environment.workspace,
                &self.snapshot_targets[column],
                path,
                text,
            );
            self.columns[column] = text.unwrap_or_default().into();
            if self.diff_history {
                self.diff_columns[column] = self.columns[column].clone();
            }
            self.snapshot_ready += 1;
        }
        self.maybe_start_diff()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing newline-bearing path, real empty blob and replacement directory remain distinct.
    #[test]
    fn batch_preserves_path_and_content_framing() {
        let oid = "a".repeat(40);
        let expressions = vec![
            "parent:line\nbreak".into(),
            "commit:b".into(),
            "commit:c".into(),
            "commit:d".into(),
        ];
        let output = format!(
            "parent:line\nbreak missing\n{oid} blob 0\n\n{oid} tree 3\nx\0y\n{oid} blob 4\na\nb\n\n"
        );
        assert_eq!(
            decode(output.as_bytes(), &expressions).unwrap(),
            [None, Some(""), None, Some("a\nb\n")]
        );
    }

    /// Truncation, binary text, submodules and extra objects never become silently empty sides.
    #[test]
    fn batch_rejects_incomplete_or_unsupported_content() {
        let oid = "b".repeat(40);
        for output in [
            format!("{oid} blob 4\nabc"),
            format!("{oid} blob 1\n\0\n"),
            format!("{oid} commit 0\n\n"),
            format!("{oid} blob 0\n\nextra"),
        ] {
            assert!(decode(output.as_bytes(), &["commit:file".into()]).is_err());
        }
    }
}
