//! What Docker is holding on to: what `docker system prune` could reclaim. Read through the `docker`
//! command, so whichever engine its current context points at (Docker Desktop, `OrbStack`, Colima) is the one asked.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::cmd::{on_path, output};

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
}
