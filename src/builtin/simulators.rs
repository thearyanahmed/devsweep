//! Xcode's simulator runtimes: 5 to 8 GB each, and every Xcode update adds one without removing the last.

use std::collections::HashMap;

use serde_json::Value;

use crate::cmd::output;
use crate::sweep::{Clean, Item, Safety};

/// One simulator runtime `xcrun simctl runtime list` knows.
#[derive(Debug, PartialEq, Eq)]
struct Runtime {
    id: String,
    /// `iOS`, `watchOS`, ...
    platform: String,
    version: String,
    bytes: u64,
    /// When a simulator last ran on it: `2024-03-02`.
    last_used: String,
}

/// The runtimes that can be deleted, as caches removed with `xcrun simctl runtime delete`. The newest of each platform,
/// which the installed Xcode builds against, is listed but not offered.
pub(super) fn runtimes() -> Vec<Item> {
    let Some(json) = output("xcrun", &["simctl", "runtime", "list", "-j"]) else {
        return Vec::new();
    };
    let list = parse(&json);
    let mut newest: HashMap<&str, &Runtime> = HashMap::new();
    for r in &list {
        newest
            .entry(&r.platform)
            .and_modify(|n| {
                if version(&r.version) > version(&n.version) {
                    *n = r;
                }
            })
            .or_insert(r);
    }
    list.iter()
        .map(|r| {
            let clean = if newest
                .get(r.platform.as_str())
                .is_some_and(|n| n.id == r.id)
            {
                Clean::Blocked(format!(
                    "the newest {} runtime, which Xcode uses",
                    r.platform
                ))
            } else {
                Clean::Tool(
                    ["xcrun", "simctl", "runtime", "delete", &r.id]
                        .map(str::to_owned)
                        .to_vec(),
                )
            };
            let note = if r.last_used.is_empty() {
                "never used".to_owned()
            } else {
                format!("last used {}", r.last_used)
            };
            Item {
                bytes: Some(r.bytes),
                safety: Safety::Caution,
                why: "downloaded again from Xcode's settings, slowly".into(),
                note,
                ..Item::new(
                    "cache",
                    &format!("{} {} simulator runtime", r.platform, r.version),
                    clean,
                )
            }
        })
        .collect()
}

/// The deletable runtimes in `xcrun simctl runtime list -j`: an object of runtimes keyed by their identifier.
fn parse(json: &str) -> Vec<Runtime> {
    let Ok(Value::Object(all)) = serde_json::from_str(json) else {
        return Vec::new();
    };
    let mut list: Vec<Runtime> = all
        .values()
        .filter(|r| r["deletable"].as_bool() == Some(true))
        .filter_map(|r| {
            // com.apple.CoreSimulator.SimRuntime.iOS-17-0: the platform is what's before the version.
            let runtime = r["runtimeIdentifier"].as_str()?;
            let platform = runtime.rsplit('.').next()?.split('-').next()?;
            Some(Runtime {
                id: r["identifier"].as_str()?.to_owned(),
                platform: platform.to_owned(),
                version: r["version"].as_str()?.to_owned(),
                bytes: r["sizeBytes"].as_u64().unwrap_or(0),
                last_used: r["lastUsedAt"]
                    .as_str()
                    .and_then(|d| d.get(..10))
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect();
    list.sort_by(|a, b| {
        a.platform
            .cmp(&b.platform)
            .then_with(|| version(&a.version).cmp(&version(&b.version)))
    });
    list
}

/// `17.0.1` as numbers, so that 17.10 sorts after 17.9.
fn version(v: &str) -> Vec<u32> {
    v.split('.').filter_map(|n| n.parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_deletable_runtimes() {
        let json = r#"{
          "A1": {"identifier": "A1", "runtimeIdentifier": "com.apple.CoreSimulator.SimRuntime.iOS-17-5",
                 "version": "17.5", "sizeBytes": 7000000000, "deletable": true,
                 "lastUsedAt": "2024-06-01T10:00:00Z"},
          "B2": {"identifier": "B2", "runtimeIdentifier": "com.apple.CoreSimulator.SimRuntime.iOS-18-0",
                 "version": "18.0", "sizeBytes": 8000000000, "deletable": true},
          "C3": {"identifier": "C3", "runtimeIdentifier": "com.apple.CoreSimulator.SimRuntime.watchOS-10-0",
                 "version": "10.0", "sizeBytes": 3000000000, "deletable": false}
        }"#;
        let list = parse(json);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].platform, "iOS");
        assert_eq!(list[0].version, "17.5");
        assert_eq!(list[0].last_used, "2024-06-01");
        assert_eq!(list[1].last_used, "");
        assert_eq!(parse("not json"), []);
        assert!(version("17.10") > version("17.9"));
    }
}
