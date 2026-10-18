//! What Docker is holding on to: its volumes, and what `docker system prune` could reclaim. Read through the `docker`
//! command, so whichever engine its current context points at (Docker Desktop, `OrbStack`, Colima) is the one asked.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::Serialize;

use crate::cmd::{on_path, output};
use crate::rules::Safety;

/// A Docker volume, and the containers that use it.
#[derive(Debug, Serialize)]
pub struct Volume {
    pub name: String,
    pub bytes: u64,
    pub containers: Vec<Container>,
    pub safety: Safety,
    pub why: String,
}

#[derive(Debug, Serialize)]
pub struct Container {
    pub name: String,
    pub running: bool,
}

impl Volume {
    /// Docker's command for removing it, once no container uses it: Docker refuses while any does, even a stopped one.
    pub fn clean_cmd(&self) -> Option<Vec<String>> {
        self.containers.is_empty().then(|| {
            vec![
                "docker".into(),
                "volume".into(),
                "rm".into(),
                self.name.clone(),
            ]
        })
    }

    /// What has to go before the volume can: its containers, stopping the running ones first.
    pub fn blocker(&self) -> String {
        let names: Vec<&str> = self.containers.iter().map(|c| c.name.as_str()).collect();
        if self.safety == Safety::Unsafe {
            format!("stop and remove {} first", names.join(", "))
        } else {
            format!("docker rm {} first", names.join(" "))
        }
    }
}

/// `docker args`, giving up after a few seconds: a stopped Docker Desktop can leave the command hanging.
fn docker(args: &'static [&'static str]) -> Option<String> {
    if !on_path("docker") {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    // Nobody may be listening by the time it answers; then the send just fails.
    thread::spawn(move || tx.send(output("docker", args)));
    rx.recv_timeout(Duration::from_secs(5)).ok().flatten()
}

/// What `docker system df` says `docker system prune` could reclaim; 0 when Docker isn't running.
pub fn reclaimable() -> u64 {
    docker(&["system", "df", "--format", "{{.Reclaimable}}"])
        .map_or(0, |out| out.lines().filter_map(|l| size(l.trim())).sum())
}

/// Every volume, biggest first (then by name), with the containers using it. Empty when Docker isn't installed or running.
///
/// A volume is never safe to remove: it holds data a container wrote, often a database. It is unsafe while a running
/// container uses it, and caution otherwise.
pub fn volumes() -> Vec<Volume> {
    let (df, ps) = thread::scope(|s| {
        let ps = s.spawn(|| docker(&["ps", "--all", "--no-trunc", "--format", "json"]));
        let df = docker(&["system", "df", "--verbose", "--format", "json"]);
        (df, ps.join().expect("the docker ps thread panicked"))
    });
    let Some(df) = df.and_then(|d| serde_json::from_str::<serde_json::Value>(&d).ok()) else {
        return Vec::new();
    };
    // One JSON object per line, one per container; Mounts names its volumes (and bind-mounted paths), comma separated.
    let containers: Vec<(String, bool, Vec<String>)> = ps
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|c| {
            let field = |k: &str| {
                c.get(k)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            };
            let mounts = field("Mounts").split(',').map(str::to_owned).collect();
            (
                field("Names").to_owned(),
                field("State") == "running",
                mounts,
            )
        })
        .collect();

    let mut volumes: Vec<Volume> = df
        .get("Volumes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let name = v.get("Name")?.as_str()?.to_owned();
            let bytes = v
                .get("Size")
                .and_then(serde_json::Value::as_str)
                .and_then(size)
                .unwrap_or(0);
            let users: Vec<Container> = containers
                .iter()
                .filter(|(_, _, mounts)| mounts.contains(&name))
                .map(|(n, running, _)| Container {
                    name: n.clone(),
                    running: *running,
                })
                .collect();
            let (safety, why) = verdict(&users);
            Some(Volume {
                name,
                bytes,
                containers: users,
                safety,
                why,
            })
        })
        .collect();
    volumes.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    volumes
}

fn verdict(users: &[Container]) -> (Safety, String) {
    let names = |running: bool| -> Vec<&str> {
        users
            .iter()
            .filter(|c| c.running == running)
            .map(|c| c.name.as_str())
            .collect()
    };
    let running = names(true);
    if !running.is_empty() {
        return (Safety::Unsafe, format!("in use by {}", running.join(", ")));
    }
    let stopped = names(false);
    if stopped.is_empty() {
        (
            Safety::Caution,
            "no container uses it, but it may still hold data".into(),
        )
    } else {
        (
            Safety::Caution,
            format!("used by stopped {}", stopped.join(", ")),
        )
    }
}

/// Parses the leading size of a line like "1.2GB (40%)", in Docker's decimal units.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "parsed from digits only, so never negative"
)]
fn size(s: &str) -> Option<u64> {
    let number = |c: char| c.is_ascii_digit() || c == '.';
    let digits: String = s.chars().take_while(|&c| number(c)).collect();
    let v: f64 = digits.parse().ok()?;
    let unit = s
        .trim_start_matches(number)
        .trim_start()
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()?;
    let mult = match unit {
        "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    Some((v * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size("1.2GB (40%)"), Some(1_200_000_000));
        assert_eq!(size("0B"), Some(0));
        assert_eq!(size("512kB"), Some(512_000));
        assert_eq!(size(""), None);
    }

    #[test]
    fn verdicts() {
        let c = |name: &str, running| Container {
            name: name.into(),
            running,
        };
        assert_eq!(verdict(&[]).0, Safety::Caution);
        assert_eq!(
            verdict(&[c("db", false)]),
            (Safety::Caution, "used by stopped db".into())
        );
        assert_eq!(
            verdict(&[c("db", false), c("api", true)]),
            (Safety::Unsafe, "in use by api".into())
        );
    }
}
