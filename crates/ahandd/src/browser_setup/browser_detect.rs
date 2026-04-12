use std::path::{Path, PathBuf};

use super::types::{BrowserKind, CheckSource, DetectedBrowser};

/// A candidate browser with its kind, path, display name, and source.
struct Candidate {
    kind: BrowserKind,
    path: &'static str,
    name: &'static str,
    source: CheckSource,
}

/// Detect a system browser. Respects `config_override` first, then falls back
/// to auto-detection with the platform-specific priority order.
pub fn detect(config_override: Option<&str>) -> Option<DetectedBrowser> {
    detect_with(config_override, &|p| Path::new(p).exists())
}

/// Detect all system browsers currently installed.
pub fn detect_all() -> Vec<DetectedBrowser> {
    detect_all_with(&|p| Path::new(p).exists())
}

fn detect_with(
    config_override: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
) -> Option<DetectedBrowser> {
    if let Some(path) = config_override {
        if exists(path) {
            return Some(DetectedBrowser {
                name: "Configured Browser".into(),
                path: PathBuf::from(path),
                kind: BrowserKind::Chrome, // conservative default for config override
                source: CheckSource::System,
            });
        }
    }

    for c in candidates() {
        if exists(c.path) {
            return Some(DetectedBrowser {
                name: c.name.into(),
                path: PathBuf::from(c.path),
                kind: c.kind.clone(),
                source: c.source.clone(),
            });
        }
    }
    None
}

fn detect_all_with(exists: &dyn Fn(&str) -> bool) -> Vec<DetectedBrowser> {
    candidates()
        .into_iter()
        .filter(|c| exists(c.path))
        .map(|c| DetectedBrowser {
            name: c.name.into(),
            path: PathBuf::from(c.path),
            kind: c.kind,
            source: c.source,
        })
        .collect()
}

/// Human-readable list of browser names tried during detection, for error messages.
pub fn tried_browsers() -> Vec<String> {
    vec!["Chrome".into(), "Chromium".into(), "Edge".into()]
}

fn candidates() -> Vec<Candidate> {
    #[cfg(target_os = "macos")]
    {
        vec![
            Candidate {
                kind: BrowserKind::Chrome,
                path: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                name: "Google Chrome",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chrome,
                path: "/Applications/Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev",
                name: "Google Chrome Dev",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chrome,
                path: "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
                name: "Google Chrome Canary",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chromium,
                path: "/Applications/Chromium.app/Contents/MacOS/Chromium",
                name: "Chromium",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Edge,
                path: "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
                name: "Microsoft Edge",
                source: CheckSource::System,
            },
        ]
    }

    #[cfg(target_os = "linux")]
    {
        vec![
            Candidate {
                kind: BrowserKind::Chrome,
                path: "/usr/bin/google-chrome-stable",
                name: "Google Chrome",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chrome,
                path: "/usr/bin/google-chrome",
                name: "Google Chrome",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chromium,
                path: "/usr/bin/chromium",
                name: "Chromium",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chromium,
                path: "/usr/bin/chromium-browser",
                name: "Chromium",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Edge,
                path: "/usr/bin/microsoft-edge-stable",
                name: "Microsoft Edge",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Edge,
                path: "/usr/bin/microsoft-edge",
                name: "Microsoft Edge",
                source: CheckSource::System,
            },
        ]
    }

    #[cfg(target_os = "windows")]
    {
        vec![
            Candidate {
                kind: BrowserKind::Chrome,
                path: r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                name: "Google Chrome",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chrome,
                path: r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
                name: "Google Chrome",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Edge,
                path: r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
                name: "Microsoft Edge",
                source: CheckSource::Preinstalled,
            },
            Candidate {
                kind: BrowserKind::Edge,
                path: r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
                name: "Microsoft Edge",
                source: CheckSource::Preinstalled,
            },
        ]
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exists_any(_: &str) -> bool {
        true
    }
    fn exists_none(_: &str) -> bool {
        false
    }

    #[test]
    fn detect_returns_first_candidate_when_all_exist() {
        let result = detect_with(None, &exists_any);
        assert!(result.is_some(), "expected some browser");
        // The first candidate varies by platform. Just verify we got something.
    }

    #[test]
    fn detect_returns_none_when_nothing_exists() {
        let result = detect_with(None, &exists_none);
        assert!(result.is_none());
    }

    #[test]
    fn detect_config_override_takes_priority_when_path_exists() {
        let result = detect_with(Some("/any/path"), &exists_any);
        assert!(result.is_some());
        let browser = result.unwrap();
        assert_eq!(browser.path, PathBuf::from("/any/path"));
        assert_eq!(browser.name, "Configured Browser");
    }

    #[test]
    fn detect_config_override_falls_back_when_path_missing() {
        // Override path doesn't exist but other candidates do.
        let exists = |p: &str| p != "/missing/override";
        let result = detect_with(Some("/missing/override"), &exists);
        assert!(result.is_some());
        assert_ne!(result.unwrap().path, PathBuf::from("/missing/override"));
    }

    #[test]
    fn detect_all_returns_multiple_when_present() {
        let all = detect_all_with(&exists_any);
        // At least one candidate exists on every platform we build for.
        assert!(!all.is_empty());
    }

    #[test]
    fn detect_all_returns_empty_when_none_exist() {
        let all = detect_all_with(&exists_none);
        assert!(all.is_empty());
    }

    #[test]
    fn tried_browsers_lists_expected_names() {
        let tried = tried_browsers();
        assert_eq!(tried, vec!["Chrome", "Chromium", "Edge"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_priority_chrome_before_edge() {
        // Only Edge exists.
        let exists = |p: &str| p.contains("Microsoft Edge");
        let result = detect_with(None, &exists);
        let browser = result.expect("expected edge");
        assert!(matches!(browser.kind, BrowserKind::Edge));

        // Both Chrome and Edge exist — Chrome wins.
        let exists_both = |p: &str| p.contains("Google Chrome") || p.contains("Microsoft Edge");
        let both = detect_with(None, &exists_both).unwrap();
        assert!(matches!(both.kind, BrowserKind::Chrome));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_edge_marked_preinstalled() {
        let exists_edge_only = |p: &str| p.contains("Edge");
        let browser = detect_with(None, &exists_edge_only).unwrap();
        assert!(matches!(browser.source, CheckSource::Preinstalled));
    }
}
