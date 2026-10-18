//! What Docker is holding on to: images, containers, volumes, and what `docker system prune` could reclaim. Read
//! through the `docker` command, so whichever engine its current context points at (Docker Desktop, `OrbStack`,
//! Colima) is the one asked.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::cmd::{on_path, output};
use crate::rules::Safety;

/// Everything Docker reported, from one `docker system df --verbose`.
#[derive(Debug, Default, Serialize)]
pub struct Snapshot {
    pub images: Vec<Image>,
    pub containers: Vec<Container>,
    pub volumes: Vec<Volume>,
}

/// A container, by name, and whether it's running.
#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub name: String,
    pub running: bool,
}

#[derive(Debug, Serialize)]
pub struct Container {
    pub name: String,
    pub running: bool,
    /// The image's name, or its short ID once no tag points to it.
    pub image: String,
    /// Its own writable layer: files it created or changed, and its logs.
    pub bytes: u64,
    pub volumes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Image {
    /// The ID's first 12 hex digits, as `docker images` shows it.
    pub id: String,
    /// repository:tag, or `<none>` for a dangling image.
    pub name: String,
    /// The layers only this image has: what removing it frees.
    pub bytes: u64,
    /// Layers it shares with other images, which stay.
    pub shared: u64,
    pub used_by: Vec<User>,
    pub safety: Safety,
    pub why: String,
}

#[derive(Debug, Serialize)]
pub struct Volume {
    pub name: String,
    pub bytes: u64,
    pub used_by: Vec<User>,
    pub safety: Safety,
    pub why: String,
}

/// An image or a volume: something Docker removes only once no container uses it, even a stopped one.
pub trait Removable {
    fn label(&self) -> &str;
    fn bytes(&self) -> u64;
    fn why(&self) -> &str;
    fn used_by(&self) -> &[User];
    fn safety(&self) -> Safety;
    /// Docker's command for removing it.
    fn remove_cmd(&self) -> Vec<String>;

    /// The command, once nothing is in the way.
    fn clean_cmd(&self) -> Option<Vec<String>> {
        self.used_by().is_empty().then(|| self.remove_cmd())
    }

    /// What has to go first: the containers using it, stopping the running ones.
    fn blocker(&self) -> String {
        let names: Vec<&str> = self.used_by().iter().map(|c| c.name.as_str()).collect();
        if self.safety() == Safety::Unsafe {
            format!("stop and remove {} first", names.join(", "))
        } else {
            format!("docker rm {} first", names.join(" "))
        }
    }
}

impl Removable for Image {
    fn label(&self) -> &str {
        &self.name
    }
    fn bytes(&self) -> u64 {
        self.bytes
    }
    fn why(&self) -> &str {
        &self.why
    }
    fn used_by(&self) -> &[User] {
        &self.used_by
    }
    fn safety(&self) -> Safety {
        self.safety
    }
    fn remove_cmd(&self) -> Vec<String> {
        // A dangling image has no name to give.
        let target = if self.name == "<none>" {
            &self.id
        } else {
            &self.name
        };
        vec!["docker".into(), "image".into(), "rm".into(), target.clone()]
    }
}

impl Removable for Volume {
    fn label(&self) -> &str {
        &self.name
    }
    fn bytes(&self) -> u64 {
        self.bytes
    }
    fn why(&self) -> &str {
        &self.why
    }
    fn used_by(&self) -> &[User] {
        &self.used_by
    }
    fn safety(&self) -> Safety {
        self.safety
    }
    fn remove_cmd(&self) -> Vec<String> {
        vec![
            "docker".into(),
            "volume".into(),
            "rm".into(),
            self.name.clone(),
        ]
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

/// Docker's images, containers and volumes, each biggest first (then by name). Empty when Docker isn't installed or
/// running.
pub fn snapshot() -> Snapshot {
    docker(&["system", "df", "--verbose", "--format", "json"])
        .and_then(|out| serde_json::from_str::<Value>(&out).ok())
        .map_or_else(Snapshot::default, |df| from_df(&df))
}

/// The entries of one of `docker system df`'s lists.
fn list<'a>(df: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    df.get(key).and_then(Value::as_array).into_iter().flatten()
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn from_df(df: &Value) -> Snapshot {
    // Every image's ID and name, which containers refer to them by.
    let names: Vec<(String, String)> = list(df, "Images")
        .map(|i| {
            let id = text(i, "ID").trim_start_matches("sha256:").to_owned();
            let name = match (text(i, "Repository"), text(i, "Tag")) {
                ("<none>" | "", _) => "<none>".to_owned(),
                (repo, "<none>" | "") => repo.to_owned(),
                (repo, tag) => format!("{repo}:{tag}"),
            };
            (id, name)
        })
        .collect();
    let containers = containers(df, &names);
    let images = images(df, &names, &containers);
    let volumes = volumes(df, &containers);
    Snapshot {
        images,
        containers,
        volumes,
    }
}

fn containers(df: &Value, names: &[(String, String)]) -> Vec<Container> {
    // A container names its image as repository:tag, or by ID once the tag has moved on to a newer build.
    let image_of = |reference: &str| -> String {
        let reference = reference.trim_start_matches("sha256:");
        names
            .iter()
            .find(|(id, name)| {
                name == reference || (!reference.is_empty() && id.starts_with(reference))
            })
            .map_or_else(|| reference.to_owned(), |(id, name)| display_name(id, name))
    };
    let mut containers: Vec<Container> = list(df, "Containers")
        .map(|c| Container {
            name: text(c, "Names").to_owned(),
            running: text(c, "State") == "running",
            image: image_of(text(c, "Image")),
            bytes: size(text(c, "Size")).unwrap_or(0),
            // Mounts names volumes and bind-mounted paths, comma separated; volume names can't hold a comma.
            volumes: text(c, "Mounts")
                .split(',')
                .filter(|m| !m.is_empty())
                .map(str::to_owned)
                .collect(),
        })
        .collect();
    containers.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    containers
}

/// The containers that `matches` picks out, as users of something.
fn users(containers: &[Container], matches: impl Fn(&Container) -> bool) -> Vec<User> {
    containers
        .iter()
        .filter(|c| matches(c))
        .map(|c| User {
            name: c.name.clone(),
            running: c.running,
        })
        .collect()
}

fn images(df: &Value, names: &[(String, String)], containers: &[Container]) -> Vec<Image> {
    let mut images: Vec<Image> = list(df, "Images")
        .zip(names)
        .map(|(i, (id, name))| {
            let shown = display_name(id, name);
            let used_by = users(containers, |c| c.image == shown);
            let (safety, why) = in_use(&used_by).unwrap_or_else(|| {
                if name == "<none>" {
                    (Safety::Safe, "dangling: no tag points to it".into())
                } else {
                    (
                        Safety::Caution,
                        "nothing uses it, but a re-pull or rebuild may differ".into(),
                    )
                }
            });
            Image {
                id: id.chars().take(12).collect(),
                name: name.clone(),
                bytes: size(text(i, "UniqueSize")).unwrap_or(0),
                shared: size(text(i, "SharedSize")).unwrap_or(0),
                used_by,
                safety,
                why,
            }
        })
        .collect();
    images.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    images
}

fn volumes(df: &Value, containers: &[Container]) -> Vec<Volume> {
    let mut volumes: Vec<Volume> = list(df, "Volumes")
        .map(|v| {
            let name = text(v, "Name").to_owned();
            let used_by = users(containers, |c| c.volumes.contains(&name));
            let (safety, why) = in_use(&used_by).unwrap_or_else(|| {
                (
                    Safety::Caution,
                    "no container uses it, but it may still hold data".into(),
                )
            });
            Volume {
                bytes: size(text(v, "Size")).unwrap_or(0),
                name,
                used_by,
                safety,
                why,
            }
        })
        .collect();
    volumes.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    volumes
}

/// An image's name as containers and listings refer to it: repository:tag, or the short ID of a dangling one.
fn display_name(id: &str, name: &str) -> String {
    if name == "<none>" {
        id.chars().take(12).collect()
    } else {
        name.to_owned()
    }
}

/// The verdict when containers use something: unsafe while one runs, caution when they're all stopped.
fn in_use(users: &[User]) -> Option<(Safety, String)> {
    let names = |running: bool| -> Vec<&str> {
        users
            .iter()
            .filter(|c| c.running == running)
            .map(|c| c.name.as_str())
            .collect()
    };
    let running = names(true);
    if !running.is_empty() {
        return Some((Safety::Unsafe, format!("in use by {}", running.join(", "))));
    }
    let stopped = names(false);
    (!stopped.is_empty()).then(|| {
        (
            Safety::Caution,
            format!("used by stopped {}", stopped.join(", ")),
        )
    })
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
        let c = |name: &str, running| User {
            name: name.into(),
            running,
        };
        assert_eq!(in_use(&[]), None);
        assert_eq!(
            in_use(&[c("db", false)]),
            Some((Safety::Caution, "used by stopped db".into()))
        );
        assert_eq!(
            in_use(&[c("db", false), c("api", true)]),
            Some((Safety::Unsafe, "in use by api".into()))
        );
    }

    #[test]
    fn reads_docker_system_df() {
        let df: Value = serde_json::from_str(
            r#"{
              "Images": [
                {"ID": "sha256:8de64586e5da562d", "Repository": "app", "Tag": "1", "UniqueSize": "7.039MB", "SharedSize": "0B"},
                {"ID": "sha256:33a0a17c27cb7253", "Repository": "<none>", "Tag": "<none>", "UniqueSize": "737B", "SharedSize": "3.029MB"},
                {"ID": "sha256:f5a7d8a177d3aa51", "Repository": "unused", "Tag": "1", "UniqueSize": "737B", "SharedSize": "3.029MB"}
              ],
              "Containers": [
                {"Names": "api", "State": "running", "Image": "app:1", "Size": "0B", "Mounts": "data"},
                {"Names": "old", "State": "exited", "Image": "33a0a17c27cb", "Size": "3MB", "Mounts": ""}
              ],
              "Volumes": [{"Name": "data", "Size": "2MB"}, {"Name": "spare", "Size": "0B"}]
            }"#,
        )
        .unwrap();
        let s = from_df(&df);
        let image = |name: &str| s.images.iter().find(|i| i.name == name).unwrap();
        assert_eq!(image("app:1").safety, Safety::Unsafe);
        // The old container refers to the dangling image by ID.
        assert_eq!(image("<none>").safety, Safety::Caution);
        assert_eq!(
            image("<none>").remove_cmd(),
            ["docker", "image", "rm", "33a0a17c27cb"]
        );
        assert_eq!(image("unused:1").safety, Safety::Caution);
        assert_eq!(
            image("unused:1").clean_cmd().unwrap(),
            ["docker", "image", "rm", "unused:1"]
        );
        assert_eq!(s.containers[0].name, "old");
        assert_eq!(s.containers[0].image, "33a0a17c27cb");
        assert_eq!(s.volumes[0].safety, Safety::Unsafe);
        assert_eq!(s.volumes[1].used_by.len(), 0);
        assert!(s.volumes[0].clean_cmd().is_none());
    }
}
