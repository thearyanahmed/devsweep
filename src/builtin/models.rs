//! Local AI models: Ollama's, one by one, and LM Studio's folder. A single model is often 4 to 40 GB.

use std::path::PathBuf;

use crate::cmd::output;
use crate::sweep::{Clean, Item, Safety, Section, Sweeper, exists};

const MODEL: &str = "model";

/// Ollama's and LM Studio's downloaded models.
#[derive(Debug)]
pub(super) struct Models;

impl Sweeper for Models {
    fn name(&self) -> &'static str {
        "models"
    }

    fn sections(&self) -> Vec<Section> {
        vec![Section {
            kind: MODEL,
            title: "Local models",
            noun: "MODEL",
            note: "(downloaded again on demand, slowly; devsweep clean -models offers each one)",
            paths: true,
            users: false,
            listed: true,
            flag: Some("models"),
            unused_title: "",
            grouped_title: "",
        }]
    }

    fn list(&self) -> Vec<Item> {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let mut items = ollama(&home);
        // LM Studio 0.3 and later keep models in ~/.lmstudio; older ones in ~/.cache/lm-studio.
        for dir in [".lmstudio/models", ".cache/lm-studio/models"] {
            let path = home.join(dir);
            if exists(&path) {
                items.push(model("lm studio models", Some(path), Clean::RemoveDir));
            }
        }
        items
    }
}

fn model(label: &str, path: Option<PathBuf>, clean: Clean) -> Item {
    Item {
        path,
        safety: Safety::Caution,
        why: "large to download again".into(),
        ..Item::new(MODEL, label, clean)
    }
}

/// Each model `ollama list` names, removed with `ollama rm`. Without a running Ollama to ask, its models folder as a
/// whole.
fn ollama(home: &std::path::Path) -> Vec<Item> {
    let dir = std::env::var_os("OLLAMA_MODELS")
        .map_or_else(|| home.join(".ollama/models"), PathBuf::from);
    // A models folder with no blobs in it holds nothing.
    if !exists(&dir.join("blobs")) {
        return Vec::new();
    }
    let Some(list) = output("ollama", &["list"]) else {
        return vec![model("ollama models", Some(dir), Clean::RemoveDir)];
    };
    parse_ollama(&list)
        .into_iter()
        .map(|(name, bytes)| Item {
            bytes: Some(bytes),
            note: "models can share layers: removing one may free less".into(),
            ..model(
                &format!("ollama {name}"),
                None,
                Clean::Tool(vec!["ollama".into(), "rm".into(), name]),
            )
        })
        .collect()
}

/// The name and size of each model in `ollama list`'s table: `NAME  ID  SIZE  MODIFIED`, the size as `4.7 GB`.
fn parse_ollama(table: &str) -> Vec<(String, u64)> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let name = cols.next()?;
            let _id = cols.next()?;
            let bytes = decimal_size(cols.next()?, cols.next()?)?;
            Some((name.to_owned(), bytes))
        })
        .collect()
}

/// `4.7`, `GB` as bytes; Ollama counts in powers of 1000.
fn decimal_size(n: &str, unit: &str) -> Option<u64> {
    let scale: u64 = match unit {
        "B" => 1,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        _ => return None,
    };
    // Whole and tenths parts apart, so no float rounding: "4.7" is 47 tenths.
    let (whole, frac) = n.split_once('.').unwrap_or((n, "0"));
    let tenths = whole.parse::<u64>().ok()? * 10 + frac.get(..1)?.parse::<u64>().ok()?;
    Some(tenths * scale / 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ollama_list() {
        let table = "NAME                ID              SIZE      MODIFIED\n\
                     llama3.1:8b         46e0c10c039e    4.9 GB    3 weeks ago\n\
                     qwen2.5-coder:32b   4bd6cbf2d094    19 GB     2 days ago\n\
                     nomic-embed-text    0a109f422b47    274 MB    5 months ago\n";
        assert_eq!(
            parse_ollama(table),
            [
                ("llama3.1:8b".to_owned(), 4_900_000_000),
                ("qwen2.5-coder:32b".to_owned(), 19_000_000_000),
                ("nomic-embed-text".to_owned(), 274_000_000),
            ]
        );
        assert_eq!(parse_ollama("NAME ID SIZE MODIFIED\n"), []);
        assert_eq!(decimal_size("1.5", "XB"), None);
    }
}
