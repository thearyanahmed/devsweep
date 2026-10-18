//! `devsweep why`: where the disk went, on one screen. The disk's size and free space, what devsweep can reclaim and
//! the command for each part, what Docker's VM disk holds back, and Time Machine's local snapshots.

use std::path::PathBuf;

use crate::builtin::{VmDisk, docker_in_use, docker_vm_disks};
use crate::cmd::output;
use crate::output::{bold, dim, human, paint, tilde};
use crate::scan::Artifact;
use crate::sweep::{Clean, Item, Safety};

/// One part of what devsweep can reclaim.
#[derive(Debug, Default)]
struct Part {
    bytes: u64,
    safe: u64,
}

impl Part {
    fn add(&mut self, bytes: u64, safety: Safety) {
        self.bytes += bytes;
        if safety == Safety::Safe {
            self.safe += bytes;
        }
    }
}

/// The disk the home folder is on: its size and what's free, in bytes.
#[derive(Debug, PartialEq, Eq)]
struct Disk {
    size: u64,
    free: u64,
}

pub(crate) fn report(roots: &[PathBuf], arts: &[Artifact], items: &[Item]) {
    let (docker_used, vms) = std::thread::scope(|s| {
        let vms = s.spawn(docker_vm_disks);
        (
            docker_in_use(),
            vms.join().expect("the VM disk thread panicked"),
        )
    });
    if let Some(d) = disk() {
        let used = d.size.saturating_sub(d.free);
        println!(
            "{}  {}, {} used, {}",
            bold("Disk"),
            human(d.size),
            human(used),
            paint("32", &format!("{} free", human(d.free)))
        );
    }

    let parts = parts(arts, items);
    let total: u64 = parts.iter().map(|(_, p, _)| p.bytes).sum();
    let safe: u64 = parts.iter().map(|(_, p, _)| p.safe).sum();
    println!(
        "\n{}  {}  {}",
        bold("Reclaimable"),
        human(total),
        dim(&format!("({} of it safe)", human(safe)))
    );
    let names: Vec<String> = roots.iter().map(|r| tilde(r)).collect();
    for (name, p, cmd) in &parts {
        if p.bytes == 0 {
            continue;
        }
        let name = if *name == "projects" && !names.is_empty() {
            format!("projects in {}", names.join(", "))
        } else {
            (*name).to_owned()
        };
        println!(
            "  {:<10}  {:<40}  {}",
            human(p.bytes),
            name,
            dim(&format!("{} safe; {cmd}", human(p.safe)))
        );
    }

    vm_report(&vms, docker_used);
    snapshots_report();
}

/// What devsweep can reclaim, by part, with the command that does it: what's unsafe or blocked doesn't count.
fn parts(arts: &[Artifact], items: &[Item]) -> Vec<(&'static str, Part, &'static str)> {
    let mut projects = Part::default();
    for a in arts
        .iter()
        .filter(|a| a.safety != Safety::Unsafe && !matches!(a.clean, Clean::Blocked(_)))
    {
        projects.add(a.freed(), a.safety);
    }
    let mut docker = Part::default();
    let mut models = Part::default();
    let mut caches = Part::default();
    let mut toolchains = Part::default();
    for i in items
        .iter()
        .filter(|i| i.safety != Safety::Unsafe && !matches!(i.clean, Clean::Blocked(_)))
    {
        let part = match i.kind.as_str() {
            "docker image" | "docker volume" => &mut docker,
            "model" => &mut models,
            "toolchain" => &mut toolchains,
            // docker system prune's share is Docker's images and volumes again, counted above.
            "cache" if !matches!(&i.clean, Clean::Tool(argv) if argv.first().is_some_and(|c| c == "docker")) => {
                &mut caches
            }
            _ => continue,
        };
        part.add(i.bytes.unwrap_or(0), i.safety);
    }
    vec![
        ("projects", projects, "devsweep clean -rm -upto caution"),
        (
            "docker images and volumes",
            docker,
            "devsweep clean -images -volumes",
        ),
        ("local models", models, "devsweep clean -models"),
        ("shared caches", caches, "devsweep clean -caches"),
        ("old toolchains", toolchains, "devsweep clean -toolchains"),
    ]
}

/// Docker's VM disk: pruning frees space inside it, which the Mac doesn't always get back.
fn vm_report(vms: &[VmDisk], docker_used: Option<u64>) {
    if vms.is_empty() {
        return;
    }
    println!("\n{}", bold("Docker's VM disk"));
    for v in vms {
        match &v.allocated {
            Ok(bytes) => println!(
                "  {:<10}  {:<40}  {}",
                human(*bytes),
                v.engine,
                dim(&tilde(&v.path))
            ),
            Err(e) => println!(
                "  {:<10}  {:<40}  {}",
                "?",
                v.engine,
                dim(&format!("can't read it: {e}"))
            ),
        }
    }
    // With one engine, Docker's own count is what's inside its disk; the difference is held back.
    if let ([v], Some(used)) = (vms, docker_used)
        && let Ok(bytes) = v.allocated
        && bytes > used
    {
        println!(
            "  {}",
            dim(&format!(
                "Docker counts {} in use: up to {} more is space Docker freed that {} hasn't given back to the disk yet",
                human(used),
                human(bytes - used),
                v.engine
            ))
        );
    }
}

/// Time Machine's local snapshots: their size can't be read without root, but macOS deletes them when space runs low.
fn snapshots_report() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let Some(list) = output("tmutil", &["listlocalsnapshots", "/"]) else {
        return;
    };
    let n = list.lines().filter(|l| l.starts_with("com.apple")).count();
    if n > 0 {
        println!(
            "\n{}  {}  {}",
            bold("Local snapshots"),
            n,
            dim(
                "(Time Machine's; their size needs root to read, and macOS deletes them when space runs low)"
            )
        );
    }
}

/// The disk the home folder is on, as `df` sees it.
fn disk() -> Option<Disk> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    output("df", &["-Pk", &home.to_string_lossy()]).and_then(|o| parse_df(&o))
}

/// The free space on the disk the home folder is on, for `clean -until`.
pub(crate) fn free_space() -> Option<u64> {
    disk().map(|d| d.free)
}

/// The size and free space in `df -Pk`'s table, from KiB.
fn parse_df(table: &str) -> Option<Disk> {
    let line = table.lines().nth(1)?;
    let mut cols = line.split_whitespace().skip(1);
    let size: u64 = cols.next()?.parse().ok()?;
    let _used = cols.next()?;
    let free: u64 = cols.next()?.parse().ok()?;
    Some(Disk {
        size: size * 1024,
        free: free * 1024,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_df() {
        let table = "Filesystem   1024-blocks     Used Available Capacity  Mounted on\n\
                     /dev/disk3s5   239311296 57200000 155200000    27%    /System/Volumes/Data\n";
        assert_eq!(
            parse_df(table),
            Some(Disk {
                size: 239_311_296 * 1024,
                free: 155_200_000 * 1024
            })
        );
        assert_eq!(parse_df("nonsense"), None);
    }
}
