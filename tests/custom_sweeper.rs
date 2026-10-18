//! Teaching devsweep something new from outside, through the public API only.
#![expect(
    unused_crate_dependencies,
    reason = "the library uses them; this test only calls it"
)]

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use devsweep::scan;
    use devsweep::sweep::{Claim, Clean, Item, Registry, Safety, Section, SimpleSweeper, Sweeper};

    /// A folder under the system temp dir, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("devsweep-it-{name}-{}", std::process::id()));
            if dir.exists() {
                fs::remove_dir_all(&dir).unwrap();
            }
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, path: &str, body: &str) {
            let p = self.0.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }

        fn git(&self, dir: &str, args: &[&str]) {
            let ok = Command::new("git")
                .arg("-C")
                .arg(self.0.join(dir))
                .args(args)
                .output()
                .unwrap();
            assert!(
                ok.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&ok.stderr)
            );
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            if let Err(e) = fs::remove_dir_all(&self.0) {
                eprintln!("couldn't remove {}: {e}", self.0.display());
            }
        }
    }

    fn gradle() -> SimpleSweeper {
        SimpleSweeper::new("gradle")
            .folders(&["build", ".gradle"])
            .markers(&["build.gradle", "build.gradle.kts"])
            .clean(&["./gradlew", "clean"])
            .rebuild("./gradlew build")
    }

    fn kinds(root: &Path, registry: &Registry) -> Vec<(String, String)> {
        let mut found: Vec<(String, String)> = scan(&[root.to_path_buf()], registry)
            .into_iter()
            .map(|a| {
                (
                    a.path.strip_prefix(root).unwrap().display().to_string(),
                    a.kind,
                )
            })
            .collect();
        found.sort();
        found
    }

    #[test]
    fn a_registered_sweeper_finds_its_folders() {
        let t = TempDir::new("gradle");
        t.write("app/build.gradle", "");
        t.write("app/build/classes/Main.class", "x");
        t.write("app/.gradle/cache", "x");
        t.write("notes/build/readme.md", "no build.gradle beside it");

        let mut registry = Registry::new();
        registry.add(gradle());
        assert_eq!(
            kinds(&t.0, &registry),
            [
                ("app/.gradle".into(), "gradle".into()),
                ("app/build".into(), "gradle".into())
            ]
        );

        let found = scan(std::slice::from_ref(&t.0), &registry);
        let build = found.iter().find(|a| a.dir == "build").unwrap();
        assert_eq!(
            build.clean,
            Clean::Tool(vec!["./gradlew".into(), "clean".into()])
        );
        assert_eq!(build.rebuild, "./gradlew build");
        assert_eq!(build.safety, Safety::Safe);
        assert!(build.bytes > 0, "the engine measures what a sweeper claims");
    }

    #[test]
    fn the_first_registered_sweeper_keeps_a_folder() {
        let t = TempDir::new("order");
        t.write("app/Cargo.toml", "");
        t.write("app/target/debug/app", "x");

        // Claims every folder named target, but comes after the built-in Rust sweeper.
        let mut registry = Registry::builtin();
        registry.add(SimpleSweeper::new("greedy").folders(&["target"]));
        assert_eq!(
            kinds(&t.0, &registry),
            [("app/target".into(), "rust".into())]
        );
    }

    /// Says everything it claims is safe, whatever is inside.
    #[derive(Debug)]
    struct Overconfident {
        dirs: Vec<String>,
    }

    impl Sweeper for Overconfident {
        fn name(&self) -> &'static str {
            "overconfident"
        }

        fn dirs(&self) -> &[String] {
            &self.dirs
        }

        fn claim(&self, _dir: &Path, _project: &Path) -> Option<Claim> {
            Some(Claim {
                safety: Safety::Safe,
                why: String::new(),
                rebuild: "nothing".into(),
                clean: Clean::RemoveDir,
            })
        }
    }

    #[test]
    fn the_safety_net_overrules_a_sweeper() {
        let t = TempDir::new("net");
        t.write("app/data/precious.db", "real data");
        t.write("app/cache/tmp", "x");
        t.git("app", &["init", "-q"]);
        t.git("app", &["add", "data"]);
        t.write("app/.gitignore", "");

        let mut registry = Registry::new();
        registry.add(Overconfident {
            dirs: vec!["data".into(), "cache".into()],
        });
        let found = scan(std::slice::from_ref(&t.0), &registry);
        let safety = |dir: &str| found.iter().find(|a| a.dir == dir).unwrap().safety;
        // Tracked in git: unsafe, whatever the sweeper said.
        assert_eq!(safety("data"), Safety::Unsafe);
        // Not tracked, but not gitignored either: at least caution.
        assert_eq!(safety("cache"), Safety::Caution);
    }

    /// Lists something found on its own, outside any project.
    #[derive(Debug)]
    struct Thumbnails;

    impl Sweeper for Thumbnails {
        fn name(&self) -> &'static str {
            "thumbnails"
        }

        fn sections(&self) -> Vec<Section> {
            vec![Section {
                kind: "thumbnail cache",
                title: "Thumbnail caches",
                noun: "CACHE",
                note: "",
                paths: true,
                users: false,
                listed: true,
                flag: None,
                unused_title: "",
                grouped_title: "",
            }]
        }

        fn list(&self) -> Vec<Item> {
            vec![Item {
                path: Some(PathBuf::from("/nonexistent/thumbnails")),
                ..Item::new("thumbnail cache", "thumbnails", Clean::RemoveDir)
            }]
        }
    }

    #[test]
    fn a_sweeper_can_list_things_outside_projects() {
        let mut registry = Registry::new();
        registry.add(Thumbnails);
        let items = Thumbnails.list();
        assert_eq!(items[0].kind, Thumbnails.sections()[0].kind);
        assert_eq!(items[0].id, "thumbnails");
        assert_eq!(items[0].safety, Safety::Safe);
        assert!(format!("{registry:?}").contains("thumbnails"));
    }
}
