//! Complete Git patch bytes are retained unchanged; a hunk is selected only within its original header.
#[derive(Default)]
pub(crate) struct Patch {
    pub bytes: Vec<u8>,
    pub header: Vec<u8>,
    pub hunks: Vec<Vec<u8>>,
    pub staged: bool,
}
impl Patch {
    /// Reject binary/oversized patches and keep newline/filename quoting exactly as emitted by Git.
    pub fn read(bytes: Vec<u8>, staged: bool) -> Result<Self, String> {
        if bytes.len() > 1024 * 1024 || bytes.windows(16).any(|w| w == b"GIT binary patch") {
            return Err("Binary or oversized patch cannot be staged by hunk".into());
        }
        let mut header = Vec::new();
        let mut hunks: Vec<Vec<u8>> = Vec::new();
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            if line.starts_with(b"@@ ") {
                hunks.push(Vec::new());
            }
            if let Some(hunk) = hunks.last_mut() {
                hunk.extend(line)
            } else {
                header.extend(line)
            }
        }
        Ok(Self {
            bytes,
            header,
            hunks,
            staged,
        })
    }
    /// Include the original full-index and path header; Git validates the preimage while locking index.
    pub fn hunk(&self, index: usize) -> Option<Vec<u8>> {
        let mut bytes = self.header.clone();
        bytes.extend(self.hunks.get(index)?);
        Some(bytes)
    }
}
