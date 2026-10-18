//! devsweep finds the build output projects leave behind (target/, `node_modules`/, .venv/, ...), says how big each one
//! is and whether deleting it is safe, and cleans it on request.

mod caches;
mod clean;
mod cmd;
mod docker;
mod output;
mod rules;
mod scan;

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process;
use std::time::{Duration, SystemTime};

use output::{
    Group, Order, SortBy, ago, bold, dim, group_heading, groups, human, plural, print_caches,
    print_docker_by_image, print_images, print_list, print_tree, print_volumes, reset,
    safety_label, summary, tilde,
};
use rules::Safety;
use scan::Artifact;

/// A command-line flag, Go style: `-name`, `--name`, `-name=value` or `-name value`.
struct Flag {
    name: &'static str,
    /// Takes a value (`-min 1G`); otherwise a switch that is on when given (`-tree`).
    takes_value: bool,
    default: &'static str,
    usage: &'static str,
}

const fn flag(name: &'static str, usage: &'static str) -> Flag {
    Flag {
        name,
        takes_value: false,
        default: "",
        usage,
    }
}

const fn string(name: &'static str, default: &'static str, usage: &'static str) -> Flag {
    Flag {
        name,
        takes_value: true,
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
        "clean: show what would run, run nothing; rm -rf is always a dry run unless --dry-run=false",
    ),
    flag(
        "include-caution",
        "clean: also offer caution artifacts, each asked one by one",
    ),
    string(
        "group",
        "",
        "group the listing by project, kind or safety; image arranges Docker by image",
    ),
    flag(
        "images",
        "only Docker images; with clean, offer each one for removal",
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
    flag(
        "reverse",
        "reverse the order: smallest, most recently built or Z to A first",
    ),
    string("sort", "size", "sort by size, age or path"),
    flag("tree", "show a tree instead of a table"),
    flag(
        "volumes",
        "only Docker volumes; with clean, offer each one for removal",
    ),
];

#[expect(
    clippy::struct_excessive_bools,
    reason = "one bool per command-line switch"
)]
#[derive(Debug)]
pub struct Options {
    pub tree: bool,
    pub json: bool,
    pub caches: bool,
    /// --dry-run as given: `None` leaves tool commands running and rm -rf as a dry run.
    pub dry_run: Option<bool>,
    pub caution: bool,
    pub rm: bool,
    pub volumes: bool,
    pub images: bool,
    /// -group image: Docker's containers and volumes under the image they come from.
    pub by_image: bool,
    pub older: Option<Duration>,
    pub min: u64,
    pub kinds: Option<HashSet<String>>,
    pub order: Order,
    pub group: Option<Group>,
    /// The string flags as given, for saying which ones hid artifacts.
    raw: BTreeMap<&'static str, String>,
}

/// The help text: what you type at full brightness, what explains it faint. A shell that leaves output faint (SGR 2)
/// gets reset first, so the plain parts really are full brightness.
fn help() -> String {
    let flag = spelled;
    let mut h = format!(
        "{}{}{}

{}
  devsweep [flags] [dirs...]         {}
  devsweep clean [flags] [dirs...]   {}

{}{}{}{}{}{}{}

{}
  {}      {}
  {}   {}
  {}    {}

{}
",
        reset(),
        bold("devsweep"),
        dim(": what your projects' build output is costing you, and cleaning it up."),
        bold("Usage:"),
        dim("list artifacts (default dir: ~/dev)"),
        dim("clean them, after asking"),
        dim(
            "Cleaning runs each tool's own command in the project (cargo clean, mix clean, swift package reset, npm run \
             clean,\ngo clean -cache, brew cleanup, docker system prune, ...). Things no tool can clean (node_modules, \
             .next, __pycache__)\nshow as \"rm -rf\" and are only removed with "
        ),
        flag("rm"),
        dim(", and even then only shown until "),
        flag("dry-run"),
        dim("=false.\nWith "),
        flag("caches"),
        dim(", shared tool caches are offered one by one too."),
        bold("Safety:"),
        safety_label(Safety::Safe),
        dim("the project's own tooling recreates it exactly (lockfile present, gitignored)"),
        safety_label(Safety::Caution),
        dim(
            "probably recreatable, but may differ or hold something you want (dist/, no lockfile, not gitignored)"
        ),
        safety_label(Safety::Unsafe),
        dim("git tracks files in it; never deleted"),
        bold("Flags:"),
    );
    // Descriptions in a column beside the flags, three spaces past the longest one, as in the sections above.
    let column = FLAGS.iter().map(|f| f.name.len() + 1).max().unwrap_or(0) + 3;
    for f in FLAGS {
        let default = if f.default.is_empty() {
            String::new()
        } else {
            format!(" (default {:?})", f.default)
        };
        let usage = dim(&format!("{}{default}", f.usage));
        writeln!(h, "  {:<column$}{usage}", flag(f.name)).expect("writing to a String can't fail");
    }
    h
}

/// Help asked for goes to stdout and exits 0; help after a mistake goes to stderr and exits 2.
fn usage(mistake: bool) -> ! {
    if mistake {
        eprint!("{}", help());
        process::exit(2)
    }
    print!("{}", help());
    process::exit(0)
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("devsweep: {msg}");
    process::exit(2)
}

/// Why the command line couldn't be used.
#[derive(Debug, PartialEq, Eq)]
enum ArgError {
    /// -h or --help: not a mistake.
    Help,
    /// A flag devsweep doesn't have, as typed.
    Unknown(String),
    /// A flag used wrongly.
    Invalid(String),
}

/// How a flag is written: one dash for a single word (-tree), two for several (--dry-run).
fn spelled(name: &str) -> String {
    if name.contains('-') {
        format!("--{name}")
    } else {
        format!("-{name}")
    }
}

/// A parsed command line.
#[derive(Debug)]
struct Args {
    /// The flags given, by name, with their values ("true" for a switch).
    set: BTreeMap<&'static str, String>,
    dirs: Vec<String>,
}

/// Parses flags anywhere among the dirs (devsweep ~/dev -tree).
fn parse_args(args: &[String]) -> Result<Args, ArgError> {
    let mut set = BTreeMap::new();
    let mut dirs = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            dirs.extend(it.by_ref().cloned());
            break;
        }
        let (double, body) = arg.strip_prefix("--").map_or_else(
            || (false, arg.strip_prefix('-').unwrap_or_default()),
            |body| (true, body),
        );
        if body.is_empty() {
            dirs.push(arg.clone());
            continue;
        }
        let (name, value) = match body.split_once('=') {
            Some((n, v)) => (n, Some(v.to_owned())),
            None => (body, None),
        };
        if name == "h" || name == "help" {
            return Err(ArgError::Help);
        }
        let Some(f) = FLAGS.iter().find(|f| f.name == name) else {
            return Err(ArgError::Unknown(
                arg.split('=').next().unwrap_or(arg).to_owned(),
            ));
        };
        let flag = spelled(f.name);
        if f.name.contains('-') && !double {
            return Err(ArgError::Invalid(format!("-{name} is written {flag}")));
        }
        let value = match (f.takes_value, value) {
            (false, None) => "true".to_owned(),
            (false, Some(v)) if matches!(v.as_str(), "true" | "false" | "1" | "0") => v,
            (false, Some(v)) => {
                return Err(ArgError::Invalid(format!(
                    "invalid boolean value {v:?} for {flag}"
                )));
            }
            (true, Some(v)) => v,
            (true, None) => it
                .next()
                .cloned()
                .ok_or_else(|| ArgError::Invalid(format!("flag needs an argument: {flag}")))?,
        };
        set.insert(f.name, value);
    }
    Ok(Args { set, dirs })
}

/// A string flag's default value.
fn default_of(name: &str) -> &'static str {
    FLAGS
        .iter()
        .find(|f| f.name == name)
        .map_or("", |f| f.default)
}

/// The options from the flags given, with defaults for the rest.
fn options(set: &BTreeMap<&'static str, String>) -> Options {
    let on = |name| set.get(name).is_some_and(|v| v == "true" || v == "1");
    let get = |name: &str| {
        set.get(name)
            .cloned()
            .unwrap_or_else(|| default_of(name).into())
    };
    let older = get("older");
    let kinds = get("kind");
    Options {
        tree: on("tree"),
        json: on("json"),
        caches: on("caches"),
        dry_run: set.get("dry-run").map(|v| v == "true" || v == "1"),
        caution: on("include-caution"),
        rm: on("rm"),
        volumes: on("volumes"),
        images: on("images"),
        by_image: get("group") == "image",
        older: (!older.is_empty()).then(|| parse_age(&older).unwrap_or_else(|e| fail(e))),
        min: parse_size(&get("min")).unwrap_or_else(|e| fail(e)),
        kinds: (!kinds.is_empty()).then(|| kinds.split(',').map(|k| k.trim().to_owned()).collect()),
        order: Order {
            by: match get("sort").as_str() {
                "size" => SortBy::Size,
                "age" => SortBy::Age,
                "path" => SortBy::Path,
                other => fail(format!("bad -sort {other:?}: use size, age or path")),
            },
            reverse: on("reverse"),
        },
        group: match get("group").as_str() {
            "" | "image" => None,
            "project" => Some(Group::Project),
            "kind" => Some(Group::Kind),
            "safety" => Some(Group::Safety),
            other => fail(format!(
                "bad -group {other:?}: use project, kind, safety or image"
            )),
        },
        raw: ["older", "kind", "min"]
            .into_iter()
            .map(|n| (n, get(n)))
            .collect(),
    }
}

/// The dirs to scan as absolute paths, ~/dev when none are given; exits if one isn't a directory.
fn resolve_roots(dirs: &[String]) -> Vec<PathBuf> {
    let dirs = if dirs.is_empty() {
        vec![Path::new(&std::env::var_os("HOME").unwrap_or_default()).join("dev")]
    } else {
        dirs.iter().map(PathBuf::from).collect()
    };
    dirs.iter()
        .map(|d| {
            let abs = normalize(&std::path::absolute(d).unwrap_or_else(|e| fail(e)));
            if !abs.is_dir() {
                fail(format!("{}: not a directory", d.display()));
            }
            abs
        })
        .collect()
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let clean = args.first().is_some_and(|a| a == "clean");
    if clean {
        args.remove(0);
    }
    let Args { set, dirs } = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(ArgError::Help) => usage(false),
        Err(ArgError::Unknown(flag)) => {
            eprintln!("flag provided but not defined: {flag}");
            usage(true)
        }
        Err(ArgError::Invalid(why)) => fail(why),
    };
    let o = options(&set);
    let roots = resolve_roots(&dirs);

    // -images or -volumes on their own: just Docker, no scan.
    if (o.images || o.volumes) && !clean && !o.json {
        print_docker(&docker::snapshot(), &o, true);
        return;
    }

    // Shared caches and Docker don't depend on the projects: look them up at the same time.
    let pending_caches = o.caches.then(|| std::thread::spawn(caches::caches));
    let caches = move || pending_caches.map(|t| t.join().expect("the cache thread panicked"));
    let pending_docker =
        (!clean || o.images || o.volumes).then(|| std::thread::spawn(docker::snapshot));
    let docker = move || {
        pending_docker.map_or_else(docker::Snapshot::default, |t| {
            t.join().expect("the docker thread panicked")
        })
    };
    let all = scan::scan(&roots);
    let shown = filter(&all, &o);

    if clean {
        let caches = caches().unwrap_or_default();
        let docker = docker();
        process::exit(clean::run_clean(&roots, &shown, &caches, &docker, &o));
    }

    if o.json {
        print_json(&roots, &shown, caches(), docker());
        return;
    }
    print_listing(&roots, &all, &shown, &o);
    print_docker(&docker(), &o, false);
    if o.caches {
        print_caches(&mut caches().unwrap_or_default());
    }
}

fn print_json(
    roots: &[PathBuf],
    shown: &[&Artifact],
    caches: Option<Vec<caches::Cache>>,
    docker: docker::Snapshot,
) {
    // Keys in the order Go's encoding/json gives a map: sorted.
    #[derive(serde::Serialize)]
    struct Out<'a> {
        artifacts: &'a [&'a Artifact],
        #[serde(skip_serializing_if = "Option::is_none")]
        caches: Option<Vec<caches::Cache>>,
        roots: &'a [PathBuf],
        #[serde(flatten)]
        docker: docker::Snapshot,
    }
    let out = Out {
        artifacts: shown,
        caches,
        roots,
        docker,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&out).expect("paths and strings always serialize")
    );
}

/// Docker's part of the listing: images and volumes, or everything by image with -group image. `only` is when
/// -images or -volumes asked for just these: then only what they name is shown, and an empty result is said.
fn print_docker(d: &docker::Snapshot, o: &Options, only: bool) {
    let images = !only || o.images;
    let volumes = !only || o.volumes;
    let mut first = only;
    let mut gap = || {
        if !std::mem::take(&mut first) {
            println!();
        }
    };
    if o.by_image && images && !d.images.is_empty() {
        gap();
        print_docker_by_image(d);
        return;
    }
    if images && !d.images.is_empty() {
        gap();
        print_images(&d.images);
    }
    if volumes && !d.volumes.is_empty() {
        gap();
        print_volumes(&d.volumes.iter().collect::<Vec<_>>(), "Docker volumes");
    }
    if only && first {
        println!("{}", dim("nothing in Docker (or Docker isn't running)"));
    }
}

/// The summary, then each root's artifacts as a table or a tree, grouped as asked.
fn print_listing(roots: &[PathBuf], all: &[Artifact], shown: &[&Artifact], o: &Options) {
    // The summary heads the listing: the note and the table or tree follow it directly.
    summary(roots, shown);
    if all.is_empty() {
        println!("{}", dim("nothing found"));
    }
    let hidden = all.len() - shown.len();
    if hidden > 0 {
        println!("{}", dim(&hidden_note(all, shown, hidden, o)));
    }
    for root in roots {
        let under: Vec<&Artifact> = shown
            .iter()
            .copied()
            .filter(|a| a.path.starts_with(root) && a.path != *root)
            .collect();
        if under.is_empty() {
            continue;
        }
        // The summary line names a single root; with several, each gets a heading.
        if roots.len() > 1 {
            let total: u64 = under.iter().map(|a| a.freed()).sum();
            println!("{}  {}", bold(&tilde(root)), human(total));
        }
        match (o.tree, o.group) {
            // The tree already groups by project, folder by folder.
            (true, Some(by @ (Group::Kind | Group::Safety))) => {
                for (name, members) in groups(root, &under, by, o.order) {
                    println!("{}", group_heading(by, &name, &members));
                    print_tree(root, &members);
                }
            }
            (true, _) => print_tree(root, &under),
            (false, group) => print_list(root, &under, group, o.order),
        }
    }
}

fn filter<'a>(all: &'a [Artifact], o: &Options) -> Vec<&'a Artifact> {
    let now = SystemTime::now();
    let mut shown: Vec<&Artifact> = all
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
    o.order.sort(&mut shown);
    shown
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
        .filter(|n| o.raw[n] != default_of(n))
        .map(|n| format!("{} {}", spelled(n), o.raw[n]))
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

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the value is checked non-negative, and a fraction of a byte doesn't matter"
)]
fn parse_size(s: &str) -> Result<u64, String> {
    let upper = s.to_uppercase();
    let s = upper.strip_suffix('B').unwrap_or(&upper);
    let s = s.strip_suffix('I').unwrap_or(s);
    let (num, mult) = ["K", "M", "G", "T"]
        .iter()
        .zip(1..)
        .find_map(|(unit, power)| Some((s.strip_suffix(unit)?, 1u64 << (10 * power))))
        .unwrap_or((s, 1));
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
        assert_eq!(parse_age("2w").unwrap(), Duration::from_hours(336));
        assert_eq!(parse_age("1mo").unwrap(), Duration::from_hours(720));
        assert_eq!(parse_age("12h").unwrap(), Duration::from_hours(12));
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
        let Args { set, dirs } = parse_args(&args).unwrap();
        assert_eq!(dirs, ["~/a", "~/b"]);
        assert_eq!(set["tree"], "true");
        assert_eq!(set["min"], "0");
        assert_eq!(set["older"], "3d");
    }

    #[test]
    fn multi_word_flags_take_two_dashes() {
        let parse =
            |args: &[&str]| parse_args(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>());
        assert_eq!(parse(&["--dry-run"]).unwrap().set["dry-run"], "true");
        assert_eq!(parse(&["--include-caution", "-rm"]).unwrap().set.len(), 2);
        assert_eq!(
            parse(&["-dry-run"]).unwrap_err(),
            ArgError::Invalid("-dry-run is written --dry-run".into())
        );
        assert_eq!(parse(&["--tree"]).unwrap().set["tree"], "true");
        assert_eq!(
            parse(&["-nope=1"]).unwrap_err(),
            ArgError::Unknown("-nope".into())
        );
        assert_eq!(parse(&["--help"]).unwrap_err(), ArgError::Help);
    }
}
