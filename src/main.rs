//! devsweep finds the build output projects leave behind (target/, node_modules/, .venv/, ...), says how big each one
//! is and whether deleting it is safe, and cleans it on request.

mod caches;
mod clean;
mod output;
mod rules;
mod scan;

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process;
use std::time::{Duration, SystemTime};

use output::{ago, dim, human, plural, print_caches, print_list, print_tree, summary};
use scan::Artifact;

const USAGE: &str = "devsweep: what your projects' build output is costing you, and cleaning it up.

Usage:
  devsweep [flags] [dirs...]         list artifacts (default dir: ~/dev)
  devsweep clean [flags] [dirs...]   clean them, after asking

Cleaning runs each tool's own command in the project (cargo clean, mix clean, swift package reset, npm run clean,
go clean -cache, brew cleanup, docker system prune, ...). Things no tool can clean (node_modules, .next, __pycache__)
show as \"rm -rf\" and are only removed with -rm. With -caches, shared tool caches are offered one by one too.

Safety:
  safe      the project's own tooling recreates it exactly (lockfile present, gitignored)
  caution   probably recreatable, but may differ or hold something you want (dist/, no lockfile, not gitignored)
  unsafe    git tracks files in it; never deleted

Flags:
";

/// A command-line flag, Go style: `-name`, `--name`, `-name=value` or `-name value`.
struct Flag {
    name: &'static str,
    /// Empty for a boolean flag.
    kind: &'static str,
    default: &'static str,
    usage: &'static str,
}

const fn flag(name: &'static str, usage: &'static str) -> Flag {
    Flag {
        name,
        kind: "",
        default: "",
        usage,
    }
}

const fn string(name: &'static str, default: &'static str, usage: &'static str) -> Flag {
    Flag {
        name,
        kind: "string",
        default,
        usage,
    }
}

// In the order -h prints them.
const FLAGS: &[Flag] = &[
    flag(
        "caches",
        "also list shared tool caches (cargo, go, npm, pnpm, brew, ...)",
    ),
    flag(
        "dry-run",
        "clean: show what would be deleted, delete nothing",
    ),
    flag(
        "include-caution",
        "clean: also offer caution artifacts, each asked one by one",
    ),
    flag("json", "print JSON"),
    string(
        "kind",
        "",
        "only these kinds, comma separated: rust,node,go,python,swift,zig,elixir,cmake",
    ),
    string(
        "min",
        "1M",
        "hide artifacts smaller than this, e.g. 100M, 1G",
    ),
    string(
        "older",
        "",
        "only artifacts not built for this long, e.g. 14d, 3w, 2mo, 12h",
    ),
    flag(
        "rm",
        "clean: also rm -rf artifacts and caches no tool has a clean command for (node_modules, .next, ...)",
    ),
    string("sort", "size", "sort by size, age or path"),
    flag("tree", "show a tree instead of a table"),
    flag("yes", "clean: don't ask before deleting safe artifacts"),
];

pub struct Options {
    pub tree: bool,
    pub json: bool,
    pub caches: bool,
    pub yes: bool,
    pub dry_run: bool,
    pub caution: bool,
    pub rm: bool,
    pub older: Option<Duration>,
    pub min: u64,
    pub kinds: Option<HashSet<String>>,
    pub sort_by: String,
    /// The string flags as given, for saying which ones hid artifacts.
    raw: BTreeMap<&'static str, String>,
}

fn usage() -> ! {
    eprint!("{USAGE}");
    for f in FLAGS {
        let kind = if f.kind.is_empty() {
            String::new()
        } else {
            format!(" {}", f.kind)
        };
        let default = if f.default.is_empty() {
            String::new()
        } else {
            format!(" (default {:?})", f.default)
        };
        eprintln!("  -{}{kind}\n    \t{}{default}", f.name, f.usage);
    }
    process::exit(0)
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("devsweep: {msg}");
    process::exit(2)
}

/// Parses flags anywhere among the dirs (devsweep ~/dev -tree), returning the flags set and the dirs.
fn parse_args(args: &[String]) -> (BTreeMap<&'static str, String>, Vec<String>) {
    let mut set = BTreeMap::new();
    let mut dirs = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            dirs.extend(it.by_ref().cloned());
            break;
        }
        let Some(body) = arg
            .strip_prefix("--")
            .or_else(|| arg.strip_prefix('-'))
            .filter(|b| !b.is_empty())
        else {
            dirs.push(arg.clone());
            continue;
        };
        let (name, value) = match body.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (body, None),
        };
        if name == "h" || name == "help" {
            usage();
        }
        let Some(f) = FLAGS.iter().find(|f| f.name == name) else {
            eprintln!("flag provided but not defined: -{name}");
            usage();
        };
        let value = match (f.kind.is_empty(), value) {
            (true, None) => "true".to_string(),
            (true, Some(v)) if matches!(v.as_str(), "true" | "false" | "1" | "0") => v,
            (true, Some(v)) => fail(format!("invalid boolean value {v:?} for -{name}")),
            (false, Some(v)) => v,
            (false, None) => it
                .next()
                .cloned()
                .unwrap_or_else(|| fail(format!("flag needs an argument: -{name}"))),
        };
        set.insert(f.name, value);
    }
    (set, dirs)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let clean = args.first().is_some_and(|a| a == "clean");
    if clean {
        args.remove(0);
    }
    let (set, dirs) = parse_args(&args);
    let on = |name| set.get(name).is_some_and(|v| v == "true" || v == "1");
    let get = |name: &str| {
        set.get(name).cloned().unwrap_or_else(|| {
            FLAGS
                .iter()
                .find(|f| f.name == name)
                .unwrap()
                .default
                .into()
        })
    };

    let older = get("older");
    let kinds = get("kind");
    let o = Options {
        tree: on("tree"),
        json: on("json"),
        caches: on("caches"),
        yes: on("yes"),
        dry_run: on("dry-run"),
        caution: on("include-caution"),
        rm: on("rm"),
        older: (!older.is_empty()).then(|| parse_age(&older).unwrap_or_else(|e| fail(e))),
        min: parse_size(&get("min")).unwrap_or_else(|e| fail(e)),
        kinds: (!kinds.is_empty())
            .then(|| kinds.split(',').map(|k| k.trim().to_string()).collect()),
        sort_by: get("sort"),
        raw: ["older", "kind", "min"]
            .into_iter()
            .map(|n| (n, get(n)))
            .collect(),
    };

    let dirs = if dirs.is_empty() {
        vec![Path::new(&std::env::var_os("HOME").unwrap_or_default()).join("dev")]
    } else {
        dirs.iter().map(PathBuf::from).collect()
    };
    let roots: Vec<PathBuf> = dirs
        .iter()
        .map(|d| {
            let abs = normalize(&std::path::absolute(d).unwrap_or_else(|e| fail(e)));
            if !abs.is_dir() {
                fail(format!("{}: not a directory", d.display()));
            }
            abs
        })
        .collect();

    let all = scan::scan(&roots);
    let arts = filter(&all, &o);

    if clean {
        let caches = if o.caches { caches::caches() } else { vec![] };
        process::exit(clean::run_clean(&roots, &arts, &caches, &o));
    }

    if o.json {
        let mut out = serde_json::json!({ "roots": roots, "artifacts": arts });
        if o.caches {
            out["caches"] = serde_json::to_value(caches::caches()).unwrap();
        }
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return;
    }

    summary(&roots, &arts);
    println!();
    if all.is_empty() {
        println!("{}", dim("nothing found"));
    }
    let hidden = all.len() - arts.len();
    if hidden > 0 {
        println!("{}\n", dim(&hidden_note(&all, &arts, hidden, &o)));
    }
    for root in &roots {
        let under: Vec<&Artifact> = arts
            .iter()
            .copied()
            .filter(|a| a.path.starts_with(root) && a.path != *root)
            .collect();
        if under.is_empty() {
            continue;
        }
        if o.tree {
            print_tree(root, &under)
        } else {
            print_list(root, &under)
        }
    }
    if o.caches {
        print_caches(&mut caches::caches());
    }
}

fn filter<'a>(all: &'a [Artifact], o: &Options) -> Vec<&'a Artifact> {
    let now = SystemTime::now();
    let mut arts: Vec<&Artifact> = all
        .iter()
        .filter(|a| a.bytes >= o.min)
        .filter(|a| {
            o.older.is_none_or(|older| {
                a.last_built
                    .is_none_or(|t| now.duration_since(t).unwrap_or_default() >= older)
            })
        })
        .filter(|a| o.kinds.as_ref().is_none_or(|k| k.contains(a.kind)))
        .collect();
    match o.sort_by.as_str() {
        "age" => arts.sort_by_key(|a| a.last_built),
        "path" => arts.sort_by(|a, b| a.path.cmp(&b.path)),
        _ => arts.sort_by_key(|a| std::cmp::Reverse(a.freed())),
    }
    arts
}

/// How many artifacts the filters left out and why, so an empty result isn't a mystery.
fn hidden_note(all: &[Artifact], shown: &[&Artifact], hidden: usize, o: &Options) -> String {
    let left_out = all
        .iter()
        .filter(|a| !shown.iter().any(|s| std::ptr::eq(*s, *a)));
    let bytes: u64 = left_out.clone().map(Artifact::freed).sum();
    let newest = left_out.filter_map(|a| a.last_built).max();
    let mut why: Vec<String> = ["older", "kind", "min"]
        .into_iter()
        .filter(|n| o.raw[n] != FLAGS.iter().find(|f| f.name == *n).unwrap().default)
        .map(|n| format!("-{n} {}", o.raw[n]))
        .collect();
    if why.is_empty() {
        why.push(format!("-min {}", o.raw["min"]));
    }
    format!(
        "{} ({}) hidden by {}; newest built {}",
        plural(hidden, "artifact"),
        human(bytes),
        why.join(", "),
        ago(newest)
    )
}

/// Resolves . and .. without touching the filesystem, like Go's filepath.Clean.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn parse_age(s: &str) -> Result<Duration, String> {
    const HOUR: f64 = 3600.0;
    const UNITS: &[(&str, f64)] = &[
        ("mo", 30.0 * 24.0 * HOUR),
        ("y", 365.0 * 24.0 * HOUR),
        ("w", 7.0 * 24.0 * HOUR),
        ("d", 24.0 * HOUR),
        ("h", HOUR),
    ];
    for (suffix, secs) in UNITS {
        if let Some(n) = s.strip_suffix(suffix) {
            if let Ok(v) = n.parse::<f64>()
                && v >= 0.0
            {
                return Ok(Duration::from_secs_f64(v * secs));
            }
            break;
        }
    }
    Err(format!("bad -older {s:?}: use e.g. 12h, 14d, 3w, 2mo, 1y"))
}

fn parse_size(s: &str) -> Result<u64, String> {
    let upper = s.to_uppercase();
    let s = upper.strip_suffix('B').unwrap_or(&upper);
    let s = s.strip_suffix('I').unwrap_or(s);
    let (num, mult) = match "KMGT".find(s.chars().last().unwrap_or(' ')) {
        Some(i) => (&s[..s.len() - 1], 1u64 << (10 * (i + 1))),
        None => (s, 1),
    };
    match num.parse::<f64>() {
        Ok(v) if v >= 0.0 => Ok((v * mult as f64) as u64),
        _ => Err("bad -min: use e.g. 500K, 100M, 1G".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ages_and_sizes() {
        assert_eq!(parse_age("2w").unwrap(), Duration::from_secs(14 * 86_400));
        assert_eq!(parse_age("1mo").unwrap(), Duration::from_secs(30 * 86_400));
        assert_eq!(parse_age("12h").unwrap(), Duration::from_secs(12 * 3600));
        assert!(parse_age("5m").is_err());
        assert_eq!(parse_size("100M").unwrap(), 100 << 20);
        assert_eq!(parse_size("1GiB").unwrap(), 1 << 30);
        assert_eq!(parse_size("0").unwrap(), 0);
        assert!(parse_size("lots").is_err());
    }

    #[test]
    fn flags_go_anywhere() {
        let args: Vec<String> = ["~/a", "-tree", "--min=0", "-older", "3d", "~/b"]
            .map(String::from)
            .to_vec();
        let (set, dirs) = parse_args(&args);
        assert_eq!(dirs, ["~/a", "~/b"]);
        assert_eq!(set["tree"], "true");
        assert_eq!(set["min"], "0");
        assert_eq!(set["older"], "3d");
    }
}
