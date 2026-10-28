//! Native diagnostics are bounded and URL credentials are removed before publication or persistence.

/// Git may echo a configured URL in errors. Never expose its userinfo in native UI or plan previews.
pub(crate) fn safe_diagnostic(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut result = String::new();
    let mut remaining = text.as_ref();
    while let Some(index) = remaining.find("://") {
        let start = index + 3;
        result.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        let end = remaining
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '/' | '\'' | '"' | '?' | '#'))
            .unwrap_or(remaining.len());
        if let Some(at) = remaining[..end].rfind('@') {
            result.push_str("[redacted]@");
            remaining = &remaining[at + 1..];
        }
        if end == 0 && !remaining.is_empty() {
            let size = remaining.chars().next().unwrap().len_utf8();
            result.push_str(&remaining[..size]);
            remaining = &remaining[size..];
        }
    }
    result.push_str(remaining);
    result.chars().take(4096).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn errors_hide_url_userinfo_and_preserve_unicode_without_truncating_code_points() {
        let error = safe_diagnostic(
            b"fatal: https://user:secret@example.invalid/repo and ssh://name:token@host/path",
        );
        assert_eq!(
            error,
            "fatal: https://[redacted]@example.invalid/repo and ssh://[redacted]@host/path"
        );
        assert!(!error.contains("secret"));
        assert!(!error.contains("token"));
        assert_eq!(safe_diagnostic("中文😀".as_bytes()), "中文😀");
    }
}
