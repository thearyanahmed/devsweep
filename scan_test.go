package main

import (
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

func TestSafety(t *testing.T) {
	root := t.TempDir()
	write := func(p, body string) {
		t.Helper()
		p = filepath.Join(root, p)
		os.MkdirAll(filepath.Dir(p), 0o755)
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	git := func(dir string, args ...string) {
		t.Helper()
		cmd := exec.Command("git", append([]string{"-C", filepath.Join(root, dir)}, args...)...)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}

	// Rust, gitignored target: safe.
	write("rs/Cargo.toml", "")
	write("rs/.gitignore", "target\n")
	write("rs/target/debug/app", "binary")
	git("rs", "init", "-q")

	// Node with a lockfile but node_modules not gitignored: caution. dist tracked in git: unsafe.
	write("web/package.json", "{}")
	write("web/package-lock.json", "{}")
	write("web/node_modules/x/index.js", "x")
	write("web/dist/logo.svg", "<svg/>")
	git("web", "init", "-q")
	git("web", "add", "dist")

	// Node without a lockfile, no git: caution.
	write("loose/package.json", "{}")
	write("loose/node_modules/y/index.js", "y")

	// A directory named target with no Cargo.toml is not claimed.
	write("docs/target/notes.md", "keep me")

	as, err := scan([]string{root})
	if err != nil {
		t.Fatal(err)
	}
	got := map[string]Safety{}
	for _, a := range as {
		r, _ := filepath.Rel(root, a.Path)
		got[r] = a.Safety
	}
	want := map[string]Safety{
		"rs/target":          Safe,
		"web/node_modules":   Caution,
		"web/dist":           Unsafe,
		"loose/node_modules": Caution,
	}
	for p, s := range want {
		if g, ok := got[p]; !ok {
			t.Errorf("%s: not found", p)
		} else if g != s {
			t.Errorf("%s: got %s, want %s", p, g, s)
		}
	}
	if _, ok := got["docs/target"]; ok {
		t.Errorf("docs/target claimed without a Cargo.toml")
	}
	if len(got) != len(want) {
		t.Errorf("found %v", got)
	}
}

func TestParse(t *testing.T) {
	if d, _ := parseAge("2w"); d.Hours() != 14*24 {
		t.Errorf("2w = %v", d)
	}
	if d, _ := parseAge("1mo"); d.Hours() != 30*24 {
		t.Errorf("1mo = %v", d)
	}
	if n, _ := parseSize("100M"); n != 100<<20 {
		t.Errorf("100M = %d", n)
	}
	if n, _ := parseSize("1GiB"); n != 1<<30 {
		t.Errorf("1GiB = %d", n)
	}
}
