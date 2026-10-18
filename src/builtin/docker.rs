//! What Docker is holding on to: images, containers, volumes, and what `docker system prune` could reclaim. Read
//! through the `docker` command, so whichever engine its current context points at (Docker Desktop, `OrbStack`,
//! Colima) is the one asked.

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::cmd::{on_path, output};
use crate::sweep::{Clean, Item, Safety, Section, Sweeper, User};

/// Everything Docker reported, from one `docker system df --verbose`.
#[derive(Debug, Default, Serialize)]
pub(super) struct Snapshot {
    pub images: Vec<Image>,
    pub containers: Vec<Container>,
    pub volumes: Vec<Volume>,
}

#[derive(Debug, Serialize)]
pub(super) struct Container {
    pub name: String,
    pub running: bool,
    /// The image's name, or its short ID once no tag points to it.
    pub image: String,
    /// Its own writable layer: files it created or changed, and its logs.
    pub bytes: u64,
    pub volumes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Image {
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
pub(super) struct Volume {
    pub name: String,
    pub bytes: u64,
    pub used_by: Vec<User>,
    pub safety: Safety,
    pub why: String,
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
pub(super) fn reclaimable() -> u64 {
    docker(&["system", "df", "--format", "{{.Reclaimable}}"])
        .map_or(0, |out| out.lines().filter_map(|l| size(l.trim())).sum())
}

/// What `docker system df` says images, containers, volumes and the build cache take; `None` when Docker isn't
/// running.
pub(super) fn total_size() -> Option<u64> {
    docker(&["system", "df", "--format", "{{.Size}}"])
        .map(|out| out.lines().filter_map(|l| size(l.trim())).sum())
}

/// The folder a Docker engine keeps its VM's disk in, and how much of the Mac's disk that takes.
#[derive(Debug)]
pub(crate) struct VmDisk {
    pub engine: &'static str,
    pub path: PathBuf,
    /// What its files take on disk: disk images are sparse, so less than their length. Why not, when the folder
    /// can't be read.
    pub allocated: Result<u64, String>,
}

/// The VM disks of the engines installed: Docker Desktop, `OrbStack`, Colima and Rancher Desktop. Pruning frees space
/// inside them, which isn't always given back to macOS.
pub(super) fn vm_disks() -> Vec<VmDisk> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let found = vec![
        (
            "Docker Desktop",
            home.join("Library/Containers/com.docker.docker/Data/vms/0/data"),
        ),
        (
            "OrbStack",
            home.join("Library/Group Containers/HUAQ24HBR6.dev.orbstack/data"),
        ),
        (
            "Rancher Desktop",
            home.join("Library/Application Support/rancher-desktop/lima/0"),
        ),
    ];
    let mut disks: Vec<VmDisk> = found
        .into_iter()
        .filter(|(_, path)| path.is_dir())
        .map(|(engine, path)| VmDisk {
            engine,
            allocated: files_size(&path),
            path,
        })
        .collect();
    // Colima keeps one Lima VM per profile, beside Lima's own _config and _disks; since v0.9 the container data is on
    // a disk of its own in _disks, named after the profile.
    let lima = home.join(".colima/_lima");
    if let Ok(profiles) = std::fs::read_dir(&lima) {
        for p in profiles
            .flatten()
            .filter(|p| !p.file_name().to_string_lossy().starts_with('_'))
        {
            let data = lima.join("_disks").join(p.file_name());
            let extra = if data.is_dir() {
                files_size(&data).unwrap_or(0)
            } else {
                0
            };
            disks.push(VmDisk {
                engine: "Colima",
                allocated: files_size(&p.path()).map(|vm| vm + extra),
                path: p.path(),
            });
        }
    }
    disks
}

/// What the files directly in `dir` take on disk.
fn files_size(dir: &std::path::Path) -> Result<u64, String> {
    use std::os::unix::fs::MetadataExt as _;
    let entries = std::fs::read_dir(dir).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            "macOS keeps the folder from your terminal; Full Disk Access lets it read it".to_owned()
        } else {
            e.to_string()
        }
    })?;
    Ok(entries
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.blocks() * 512)
        .sum())
}

/// Docker's images, containers and volumes, each biggest first (then by name). Empty when Docker isn't installed or
/// running.
pub(super) fn snapshot() -> Snapshot {
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
    fn finds_kind_and_k3d_nodes() {
        let node = |name: &str, image: &str| Container {
            name: name.into(),
            running: true,
            image: image.into(),
            bytes: 0,
            volumes: Vec::new(),
        };
        assert_eq!(
            cluster_of(&node("dev-control-plane", "kindest/node:v1.30.0")),
            Some("kind dev".into())
        );
        assert_eq!(
            cluster_of(&node("dev-worker2", "kindest/node:v1.30.0")),
            Some("kind dev".into())
        );
        assert_eq!(
            cluster_of(&node(
                "dev-external-load-balancer",
                "kindest/haproxy:v20230606"
            )),
            Some("kind dev".into())
        );
        assert_eq!(
            cluster_of(&node("k3d-lab-server-0", "rancher/k3s:v1.30")),
            Some("k3d lab".into())
        );
        assert_eq!(
            cluster_of(&node("k3d-lab-serverlb", "ghcr.io/k3d-io/k3d-proxy")),
            Some("k3d lab".into())
        );
        assert_eq!(cluster_of(&node("api", "postgres:16")), None);
        assert_eq!(
            clusters(&[
                node("dev-control-plane", "kindest/node"),
                node("dev-worker", "kindest/node")
            ])[0]
                .clean,
            Clean::Tool(
                ["kind", "delete", "cluster", "--name", "dev"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
    }

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
        let items = items(from_df(&df));
        let item = |kind: &str, label: &str| {
            items
                .iter()
                .find(|i| i.kind == kind && i.label == label)
                .unwrap()
        };
        let tool = |argv: &[&str]| Clean::Tool(argv.iter().map(|a| (*a).to_owned()).collect());
        assert_eq!(item(IMAGE, "app:1").safety, Safety::Unsafe);
        assert_eq!(
            item(IMAGE, "app:1").clean,
            Clean::Blocked("stop and remove api first".into())
        );
        // The old container refers to the dangling image by ID; it's in the way of removing it.
        let dangling = item(IMAGE, "<none>");
        assert_eq!(dangling.id, "33a0a17c27cb");
        assert_eq!(dangling.safety, Safety::Caution);
        assert_eq!(dangling.clean, Clean::Blocked("docker rm old first".into()));
        assert_eq!(
            item(IMAGE, "unused:1").clean,
            tool(&["docker", "image", "rm", "unused:1"])
        );
        let old = item(CONTAINER, "old");
        assert_eq!(old.parent.as_deref(), Some("33a0a17c27cb"));
        assert_eq!(item(CONTAINER, "api").uses, ["data"]);
        assert_eq!(item(VOLUME, "data").safety, Safety::Unsafe);
        assert_eq!(
            item(VOLUME, "spare").clean,
            tool(&["docker", "volume", "rm", "spare"])
        );
    }
}

/// Docker's images, containers and volumes, as items. Containers only show under their image, with `-group image`.
#[derive(Debug)]
pub(super) struct Docker;

const IMAGE: &str = "docker image";
const CONTAINER: &str = "docker container";
const VOLUME: &str = "docker volume";
const CLUSTER: &str = "local cluster";

impl Sweeper for Docker {
    fn name(&self) -> &'static str {
        "docker"
    }

    fn sections(&self) -> Vec<Section> {
        vec![
            Section {
                kind: IMAGE,
                title: "Docker images",
                noun: "IMAGE",
                note: "(shared layers counted once; devsweep clean -images offers each one)",
                paths: false,
                users: true,
                listed: true,
                flag: Some("images"),
                unused_title: "",
                grouped_title: "Docker by image",
            },
            Section {
                kind: CONTAINER,
                title: "Docker containers",
                noun: "CONTAINER",
                note: "",
                paths: false,
                users: false,
                listed: false,
                flag: None,
                unused_title: "",
                grouped_title: "",
            },
            Section {
                kind: VOLUME,
                title: "Docker volumes",
                noun: "VOLUME",
                note: "(data containers wrote; devsweep clean -volumes offers each one)",
                paths: false,
                users: true,
                listed: true,
                flag: Some("volumes"),
                unused_title: "Docker volumes no container uses",
                grouped_title: "",
            },
            Section {
                kind: CLUSTER,
                title: "Local Kubernetes clusters",
                noun: "CLUSTER",
                note: "(kind and k3d; devsweep clean -clusters deletes each one with its own tool)",
                paths: false,
                users: false,
                listed: true,
                flag: Some("clusters"),
                unused_title: "",
                grouped_title: "",
            },
        ]
    }

    fn list(&self, _roots: &[PathBuf]) -> Vec<Item> {
        items(snapshot())
    }
}

/// A snapshot's images, containers and volumes, as items.
fn items(s: Snapshot) -> Vec<Item> {
    let clusters = clusters(&s.containers);
    let images = s.images.into_iter().map(|i| {
        let id = display_name(&i.id, &i.name);
        removable(Item {
            id,
            bytes: Some(i.bytes),
            shared: i.shared,
            safety: i.safety,
            why: i.why,
            used_by: i.used_by,
            ..Item::new(IMAGE, &i.name, Clean::Tool(Vec::new()))
        })
    });
    let containers = s.containers.into_iter().map(|c| Item {
        bytes: Some(c.bytes),
        running: Some(c.running),
        parent: Some(c.image),
        uses: c.volumes,
        ..Item::new(
            CONTAINER,
            &c.name,
            Clean::Blocked("devsweep never removes a container".into()),
        )
    });
    let volumes = s.volumes.into_iter().map(|v| {
        removable(Item {
            bytes: Some(v.bytes),
            safety: v.safety,
            why: v.why,
            used_by: v.used_by,
            ..Item::new(VOLUME, &v.name, Clean::Tool(Vec::new()))
        })
    });
    images
        .chain(containers)
        .chain(volumes)
        .chain(clusters)
        .collect()
}

/// The kind and k3d clusters among the containers, each removed with its own tool: devsweep never removes a container
/// itself, and deleting a node container alone would leave the cluster broken.
fn clusters(containers: &[Container]) -> Vec<Item> {
    let mut by: Vec<(String, Vec<&Container>)> = Vec::new();
    for c in containers {
        let Some(id) = cluster_of(c) else { continue };
        match by.iter_mut().find(|(i, _)| *i == id) {
            Some((_, nodes)) => nodes.push(c),
            None => by.push((id, vec![c])),
        }
    }
    by.into_iter()
        .map(|(id, nodes)| {
            let (tool, name) = id.split_once(' ').unwrap_or((&id, ""));
            let argv = if tool == "kind" {
                vec!["kind", "delete", "cluster", "--name", name]
            } else {
                vec!["k3d", "cluster", "delete", name]
            };
            Item {
                bytes: Some(nodes.iter().map(|n| n.bytes).sum()),
                safety: Safety::Caution,
                why: "deletes the whole cluster and what runs in it".into(),
                note: format!("{} node containers", nodes.len()),
                running: Some(nodes.iter().any(|n| n.running)),
                ..Item::new(
                    CLUSTER,
                    &id,
                    Clean::Tool(argv.into_iter().map(str::to_owned).collect()),
                )
            }
        })
        .collect()
}

/// Which cluster a container is a node of, as `kind <name>` or `k3d <name>`: kind's nodes run its `kindest/` images (`node`, and `haproxy` for the load
/// balancer)
/// image and are named `<name>-control-plane`, `<name>-worker2`, ...; k3d's are named `k3d-<name>-server-0`, ...
fn cluster_of(c: &Container) -> Option<String> {
    if c.image.starts_with("kindest/") {
        let name = ["-control-plane", "-worker", "-external-load-balancer"]
            .iter()
            .find_map(|role| c.name.split_once(role).map(|(name, _)| name))?;
        return Some(format!("kind {name}"));
    }
    let rest = c.name.strip_prefix("k3d-")?;
    let name = ["-server-", "-agent-", "-serverlb"]
        .iter()
        .find_map(|role| rest.split_once(role).map(|(name, _)| name))?;
    Some(format!("k3d {name}"))
}

/// Sets how an image or volume is removed: Docker's command once no container uses it, which Docker insists on even
/// for a stopped one; until then, the containers in the way.
fn removable(mut item: Item) -> Item {
    item.clean = if item.used_by.is_empty() {
        let what = if item.kind == IMAGE {
            "image"
        } else {
            "volume"
        };
        Clean::Tool(vec![
            "docker".into(),
            what.into(),
            "rm".into(),
            item.id.clone(),
        ])
    } else {
        let names: Vec<&str> = item.used_by.iter().map(|c| c.name.as_str()).collect();
        Clean::Blocked(if item.safety == Safety::Unsafe {
            format!("stop and remove {} first", names.join(", "))
        } else {
            format!("docker rm {} first", names.join(" "))
        })
    };
    item
}
