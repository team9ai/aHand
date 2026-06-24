//! Sandbox capability SID helpers for Windows.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rand::RngCore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CapabilitySid {
    sid_string: String,
}

impl CapabilitySid {
    pub(super) fn sid_string(&self) -> &str {
        &self.sid_string
    }
}

pub(super) fn capability_for_root(root: &Path) -> io::Result<CapabilitySid> {
    let canonical_root = root.canonicalize()?;
    let path = cap_sid_file(&canonical_root);
    if path.exists() {
        let sid_string = fs::read_to_string(&path)?.trim().to_string();
        if is_capability_sid(&sid_string) {
            return Ok(CapabilitySid { sid_string });
        }
    }

    let sid_string = make_random_cap_sid_string();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, &sid_string)?;
    Ok(CapabilitySid { sid_string })
}

fn cap_sid_file(root: &Path) -> PathBuf {
    root.join(".ahand-sandbox").join("cap_sid")
}

fn make_random_cap_sid_string() -> String {
    let mut rng = rand::thread_rng();
    format!(
        "S-1-5-21-{}-{}-{}-{}",
        rng.next_u32(),
        rng.next_u32(),
        rng.next_u32(),
        rng.next_u32()
    )
}

fn is_capability_sid(sid: &str) -> bool {
    sid.strip_prefix("S-1-5-21-")
        .map(|rest| {
            let parts = rest.split('-').collect::<Vec<_>>();
            parts.len() == 4 && parts.iter().all(|part| part.parse::<u32>().is_ok())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_for_root_is_stable_and_persisted() {
        let temp = tempfile::tempdir().unwrap();
        let first = capability_for_root(temp.path()).unwrap();
        let second = capability_for_root(temp.path()).unwrap();

        assert_eq!(first.sid_string(), second.sid_string());
        assert!(first.sid_string().starts_with("S-1-5-21-"));
        assert!(temp.path().join(".ahand-sandbox").join("cap_sid").exists());
    }

    #[test]
    fn equivalent_root_spellings_share_capability_sid() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let alternate = root.join("..").join("workspace");

        let first = capability_for_root(&root).unwrap();
        let second = capability_for_root(&alternate).unwrap();

        assert_eq!(first.sid_string(), second.sid_string());
    }
}
