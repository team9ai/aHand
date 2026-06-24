//! Sandbox capability SID helpers for Windows.
#![cfg_attr(not(test), allow(dead_code))]

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rand::RngCore;

static CAPABILITY_SID_FILE_LOCK: Mutex<()> = Mutex::new(());

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
    let _guard = CAPABILITY_SID_FILE_LOCK
        .lock()
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "capability SID lock poisoned"))?;
    capability_for_canonical_root(&canonical_root)
}

fn capability_for_canonical_root(canonical_root: &Path) -> io::Result<CapabilitySid> {
    let path = cap_sid_file(&canonical_root);

    loop {
        if let Some(sid_string) = read_valid_cap_sid_file(&path)? {
            return Ok(CapabilitySid { sid_string });
        }

        if path.exists() {
            remove_invalid_cap_sid_file(&path)?;
            continue;
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let sid_string = make_random_cap_sid_string();
        match write_new_cap_sid_file(&path, &sid_string) {
            Ok(()) => return Ok(CapabilitySid { sid_string }),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
}

fn cap_sid_file(root: &Path) -> PathBuf {
    root.join(".ahand-sandbox").join("cap_sid")
}

fn read_valid_cap_sid_file(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let sid_string = contents.trim().to_string();
            Ok(is_capability_sid(&sid_string).then_some(sid_string))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn remove_invalid_cap_sid_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn write_new_cap_sid_file(path: &Path, sid_string: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(sid_string.as_bytes())
}

/// Codex-aligned restricted-token capability SID material, not an AppContainer SID.
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
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier};

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

    #[test]
    fn invalid_persisted_capability_sid_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let cap_file = temp.path().join(".ahand-sandbox").join("cap_sid");
        std::fs::create_dir_all(cap_file.parent().unwrap()).unwrap();
        std::fs::write(&cap_file, "not-a-capability-sid").unwrap();

        let capability = capability_for_root(temp.path()).unwrap();
        let persisted = std::fs::read_to_string(&cap_file).unwrap();

        assert!(is_capability_sid(capability.sid_string()));
        assert_eq!(persisted.trim(), capability.sid_string());
        assert_ne!(persisted.trim(), "not-a-capability-sid");
    }

    #[test]
    fn concurrent_first_use_returns_one_stable_capability_sid() {
        const ROUNDS: usize = 12;
        const WORKERS: usize = 64;

        for _ in 0..ROUNDS {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("workspace");
            std::fs::create_dir_all(&root).unwrap();
            let barrier = Arc::new(Barrier::new(WORKERS));
            let mut handles = Vec::with_capacity(WORKERS);

            for _ in 0..WORKERS {
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    capability_for_root(&root).unwrap().sid_string().to_string()
                }));
            }

            let sids = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>();
            let unique = sids.iter().cloned().collect::<HashSet<_>>();
            let persisted =
                std::fs::read_to_string(root.join(".ahand-sandbox").join("cap_sid")).unwrap();

            assert_eq!(unique.len(), 1, "concurrent callers returned {unique:?}");
            assert_eq!(persisted.trim(), sids[0]);
        }
    }
}
