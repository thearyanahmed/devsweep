//! Old toolchains: the Rust, Node, Python and Java versions version managers keep installed long after the last
//! project stopped using them. A version is offered only when it isn't the default, isn't the one on PATH, and no
//! project under the scanned folders pins it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cmd::output;
use crate::output::tilde;
use crate::scan::skipped;
use crate::sweep::{Clean, Item, Safety, Section, Sweeper, exists};

const TOOLCHAIN: &str = "toolchain";

/// How deep under a scanned folder to look for files that pin a version.
const PIN_DEPTH: usize = 6;

/// The versions rustup, nvm, fnm, volta, pyenv, mise and SDKMAN have installed.
#[derive(Debug)]
pub(super) struct Toolchains;

impl Sweeper for Toolchains {
    fn name(&self) -> &'static str {
        "toolchains"
    }

    fn by_default(&self) -> bool {
        false
    }

    fn sections(&self) -> Vec<Section> {
        vec![Section {
            kind: TOOLCHAIN,
            title: "Toolchains",
            noun: "TOOLCHAIN",
            note: "(the default, the one on PATH and pinned ones are kept; devsweep clean -toolchains offers the rest)",
            paths: true,
            users: false,
            listed: true,
            flag: Some("toolchains"),
            unused_title: "",
            grouped_title: "",
        }]
    }

    fn list(&self, roots: &[PathBuf]) -> Vec<Item> {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let installed = installed(&home);
        if installed.is_empty() {
            return Vec::new();
        }
        let mut pins = pins(roots);
        for (tool, file) in global_pins(&home) {
            pins.entry(tool.0)
                .or_default()
                .push(Pin { spec: tool.1, file });
        }
        let on_path = on_path_versions();
        installed
            .into_iter()
            .map(|i| {
                let keep = if i.default {
                    Some("the default".to_owned())
                } else if on_path
                    .iter()
                    .any(|(t, v)| *t == i.tool && matches(&i.version, v))
                {
                    Some("the one on PATH".to_owned())
                } else {
                    pins.get(&i.tool)
                        .and_then(|ps| ps.iter().find(|p| matches(&i.version, &p.spec)))
                        .map(|p| format!("pinned by {}", tilde(&p.file)))
                };
                // A kept one says only why it's kept.
                let why = if keep.is_some() {
                    String::new()
                } else {
                    "a project outside the scanned folders may need it; reinstalling takes a while"
                        .into()
                };
                Item {
                    path: Some(i.path),
                    safety: Safety::Caution,
                    why,
                    ..Item::new(
                        TOOLCHAIN,
                        &format!("{} {} ({})", i.tool, i.version, i.manager),
                        keep.map_or(i.clean, Clean::Blocked),
                    )
                }
            })
            .collect()
    }
}

/// One installed version.
#[derive(Debug)]
struct Installed {
    manager: &'static str,
    /// `rust`, `node`, `python`, `java`, ...: what pins name.
    tool: String,
    version: String,
    path: PathBuf,
    clean: Clean,
    /// The version manager's default.
    default: bool,
}

/// A version a file asks for, and the file.
#[derive(Debug)]
struct Pin {
    spec: String,
    file: PathBuf,
}

/// Whether `version` (`v20.11.0`, `1.75.0-aarch64-apple-darwin`) is what `spec` (`20`, `1.75`, `stable`) asks for.
/// A partial spec matches every version it could mean: keeping one too many beats removing one in use.
fn matches(version: &str, spec: &str) -> bool {
    let v = version.trim_start_matches('v');
    let s = spec.trim().trim_start_matches('v');
    !s.is_empty() && (v == s || v.starts_with(&format!("{s}.")) || v.starts_with(&format!("{s}-")))
}

/// What a version file calls a tool, as the version managers' folders do.
fn tool_name(name: &str) -> String {
    match name {
        "nodejs" => "node".into(),
        "golang" => "go".into(),
        other => other.to_owned(),
    }
}

fn env_dir(var: &str, default: PathBuf) -> PathBuf {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map_or(default, PathBuf::from)
}

/// The real folders in `dir`, by name: symlinks (aliases like `20` or `current`) left out.
fn subdirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(n, _)| !n.starts_with('.'))
        .collect();
    dirs.sort();
    dirs
}

fn argv(parts: &[&str]) -> Clean {
    Clean::Tool(parts.iter().map(|p| (*p).to_owned()).collect())
}

fn installed(home: &Path) -> Vec<Installed> {
    let mut all = rustup(home);
    all.extend(nvm(home));
    all.extend(fnm(home));
    all.extend(volta(home));
    all.extend(pyenv(home));
    all.extend(mise(home));
    all.extend(sdkman(home));
    all
}

fn rustup(home: &Path) -> Vec<Installed> {
    let base = env_dir("RUSTUP_HOME", home.join(".rustup"));
    let settings = fs::read_to_string(base.join("settings.toml")).unwrap_or_default();
    let default = settings
        .lines()
        .find_map(|l| l.trim().strip_prefix("default_toolchain"))
        .map(|v| {
            v.trim_start_matches([' ', '='])
                .trim()
                .trim_matches('"')
                .to_owned()
        })
        .unwrap_or_default();
    subdirs(&base.join("toolchains"))
        .into_iter()
        .map(|(name, path)| Installed {
            manager: "rustup",
            tool: "rust".into(),
            default: !default.is_empty() && matches(&name, &default),
            clean: argv(&["rustup", "toolchain", "uninstall", &name]),
            version: name,
            path,
        })
        .collect()
}

fn nvm(home: &Path) -> Vec<Installed> {
    let base = env_dir("NVM_DIR", home.join(".nvm"));
    let default = fs::read_to_string(base.join("alias/default")).unwrap_or_default();
    subdirs(&base.join("versions/node"))
        .into_iter()
        .map(|(v, path)| Installed {
            manager: "nvm",
            tool: "node".into(),
            default: matches(&v, default.trim()),
            // nvm is a shell function, which devsweep can't run.
            clean: Clean::RemoveDir,
            version: v,
            path,
        })
        .collect()
}

fn fnm(home: &Path) -> Vec<Installed> {
    let candidates = [
        env_dir("FNM_DIR", home.join(".local/share/fnm")),
        home.join("Library/Application Support/fnm"),
        home.join(".fnm"),
    ];
    let Some(base) = candidates
        .into_iter()
        .find(|b| exists(&b.join("node-versions")))
    else {
        return Vec::new();
    };
    let default = fs::read_link(base.join("aliases/default")).unwrap_or_default();
    subdirs(&base.join("node-versions"))
        .into_iter()
        .map(|(v, path)| Installed {
            manager: "fnm",
            tool: "node".into(),
            default: default.starts_with(&path),
            clean: argv(&["fnm", "uninstall", &v]),
            version: v,
            path,
        })
        .collect()
}

fn volta(home: &Path) -> Vec<Installed> {
    let base = env_dir("VOLTA_HOME", home.join(".volta"));
    let platform = fs::read_to_string(base.join("tools/user/platform.json")).unwrap_or_default();
    let default = serde_json::from_str::<serde_json::Value>(&platform)
        .ok()
        .and_then(|p| p["node"]["runtime"].as_str().map(str::to_owned))
        .unwrap_or_default();
    subdirs(&base.join("tools/image/node"))
        .into_iter()
        .map(|(v, path)| Installed {
            manager: "volta",
            tool: "node".into(),
            default: v == default,
            clean: Clean::RemoveDir,
            version: v,
            path,
        })
        .collect()
}

fn pyenv(home: &Path) -> Vec<Installed> {
    let base = env_dir("PYENV_ROOT", home.join(".pyenv"));
    let global = fs::read_to_string(base.join("version")).unwrap_or_default();
    subdirs(&base.join("versions"))
        .into_iter()
        .map(|(v, path)| Installed {
            manager: "pyenv",
            tool: "python".into(),
            default: global.lines().any(|g| g.trim() == v),
            clean: argv(&["pyenv", "uninstall", "-f", &v]),
            version: v,
            path,
        })
        .collect()
}

fn mise(home: &Path) -> Vec<Installed> {
    let base = env_dir("MISE_DATA_DIR", home.join(".local/share/mise"));
    subdirs(&base.join("installs"))
        .into_iter()
        .flat_map(|(tool, dir)| {
            subdirs(&dir).into_iter().map(move |(v, path)| Installed {
                manager: "mise",
                tool: tool_name(&tool),
                // Its global versions are pins of ~/.config/mise/config.toml, read with the others.
                default: false,
                clean: argv(&["mise", "uninstall", &format!("{tool}@{v}")]),
                version: v,
                path,
            })
        })
        .collect()
}

fn sdkman(home: &Path) -> Vec<Installed> {
    let base = env_dir("SDKMAN_DIR", home.join(".sdkman"));
    subdirs(&base.join("candidates"))
        .into_iter()
        .flat_map(|(tool, dir)| {
            let current = fs::read_link(dir.join("current")).unwrap_or_default();
            subdirs(&dir).into_iter().map(move |(v, path)| Installed {
                manager: "sdkman",
                tool: tool.clone(),
                default: current
                    .file_name()
                    .is_some_and(|c| c.to_string_lossy() == v),
                // sdk is a shell function, which devsweep can't run.
                clean: Clean::RemoveDir,
                version: v,
                path,
            })
        })
        .collect()
}

/// The versions the `node` and `python3` on PATH are.
fn on_path_versions() -> Vec<(String, String)> {
    let mut found = Vec::new();
    if let Some(v) = output("node", &["--version"]) {
        found.push(("node".to_owned(), v.trim_start_matches('v').to_owned()));
    }
    if let Some(v) = output("python3", &["--version"]) {
        found.push((
            "python".to_owned(),
            v.trim_start_matches("Python ").to_owned(),
        ));
    }
    found
}

/// The versions mise's global config asks for.
fn global_pins(home: &Path) -> Vec<((String, String), PathBuf)> {
    let file = home.join(".config/mise/config.toml");
    let text = fs::read_to_string(&file).unwrap_or_default();
    tools_table(&text)
        .into_iter()
        .map(|pin| (pin, file.clone()))
        .collect()
}

/// The versions every version file under `roots` pins, by tool.
fn pins(roots: &[PathBuf]) -> HashMap<String, Vec<Pin>> {
    let mut found = HashMap::new();
    for root in roots {
        walk(root, 0, &mut found);
    }
    found
}

fn walk(dir: &Path, depth: usize, found: &mut HashMap<String, Vec<Pin>>) {
    const SKIP: &[&str] = &[
        "node_modules",
        "target",
        "vendor",
        "dist",
        "build",
        "venv",
        "Library",
    ];
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            if depth < PIN_DEPTH
                && !name.starts_with('.')
                && !SKIP.contains(&name.as_str())
                && !skipped(dir, &name)
            {
                walk(&e.path(), depth + 1, found);
            }
            continue;
        }
        let Some(read) = pin_reader(&name) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(e.path()) else {
            continue;
        };
        for (tool, spec) in read(&text) {
            found.entry(tool).or_default().push(Pin {
                spec,
                file: e.path(),
            });
        }
    }
}

type PinReader = fn(&str) -> Vec<(String, String)>;

/// How to read the versions a file of this name pins, when it's a version file.
fn pin_reader(name: &str) -> Option<PinReader> {
    Some(match name {
        "rust-toolchain" => |t| {
            first_line(t)
                .map(|v| ("rust".to_owned(), v))
                .into_iter()
                .collect()
        },
        "rust-toolchain.toml" => |t| {
            t.lines()
                .find_map(|l| l.trim().strip_prefix("channel"))
                .map(|v| ("rust".to_owned(), unquote(v.trim_start_matches([' ', '=']))))
                .into_iter()
                .collect()
        },
        ".nvmrc" | ".node-version" => |t| {
            first_line(t)
                .map(|v| ("node".to_owned(), v))
                .into_iter()
                .collect()
        },
        ".python-version" => |t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(|l| ("python".to_owned(), l.to_owned()))
                .collect()
        },
        ".tool-versions" => |t| {
            t.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .flat_map(|l| {
                    let mut words = l.split_whitespace();
                    let tool = words.next().map(tool_name).unwrap_or_default();
                    words.map(move |v| (tool.clone(), v.to_owned()))
                })
                .collect()
        },
        ".sdkmanrc" => |t| {
            t.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .filter_map(|l| l.split_once('='))
                .map(|(c, v)| (c.trim().to_owned(), v.trim().to_owned()))
                .collect()
        },
        "mise.toml" | ".mise.toml" => tools_table,
        _ => return None,
    })
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches(['"', '\'']).to_owned()
}

/// The versions in a mise config's `[tools]` table: `node = "20"`, or a list of them.
fn tools_table(text: &str) -> Vec<(String, String)> {
    let mut in_tools = false;
    let mut found = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') && !line.contains('=') {
            in_tools = line == "[tools]";
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| in_tools) else {
            continue;
        };
        let tool = tool_name(&unquote(key));
        let value = value.trim();
        // A table of options ({ version = "20", ... }) isn't read.
        if value.starts_with('{') {
            continue;
        }
        for v in value.trim_matches(['[', ']']).split(',') {
            let v = unquote(v);
            if !v.is_empty() {
                found.push((tool.clone(), v));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_match_the_versions_they_could_mean() {
        assert!(matches("v20.11.0", "20"));
        assert!(matches("20.11.0", "v20.11"));
        assert!(!matches("v20.11.0", "2"));
        assert!(matches("1.75.0-aarch64-apple-darwin", "1.75.0"));
        assert!(matches("stable-aarch64-apple-darwin", "stable"));
        assert!(!matches("nightly-aarch64-apple-darwin", "stable"));
        assert!(matches("17.0.2-tem", "17.0.2-tem"));
        assert!(!matches("3.11.4", ""));
    }

    #[test]
    fn reads_version_files() {
        let read = |name: &str, text: &str| pin_reader(name).unwrap()(text);
        assert_eq!(
            read(".nvmrc", "v20.11.0\n"),
            [("node".into(), "v20.11.0".into())]
        );
        assert_eq!(
            read("rust-toolchain.toml", "[toolchain]\nchannel = \"1.75.0\"\n"),
            [("rust".into(), "1.75.0".into())]
        );
        assert_eq!(
            read(".tool-versions", "nodejs 20.1.0 18.0.0\npython 3.11\n"),
            [
                ("node".into(), "20.1.0".into()),
                ("node".into(), "18.0.0".into()),
                ("python".into(), "3.11".into())
            ]
        );
        assert_eq!(
            read(".sdkmanrc", "# java\njava=17.0.2-tem\n"),
            [("java".into(), "17.0.2-tem".into())]
        );
        assert_eq!(
            read(
                "mise.toml",
                "[env]\nA = \"1\"\n[tools]\nnode = \"22\"\npython = [\"3.12\", \"3.11\"]\ngo = { version = \"1.22\" }\n"
            ),
            [
                ("node".into(), "22".into()),
                ("python".into(), "3.12".into()),
                ("python".into(), "3.11".into())
            ]
        );
        assert!(pin_reader("Cargo.toml").is_none());
    }
}
