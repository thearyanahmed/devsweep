// devsweep finds the build output projects leave behind (target/, node_modules/, .venv/, ...), says how big each one
// is and whether deleting it is safe, and deletes it on request.
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
)

const usage = `devsweep: what your projects' build output is costing you, and cleaning it up.

Usage:
  devsweep [flags] [dirs...]         list artifacts (default dir: ~/dev)
  devsweep clean [flags] [dirs...]   clean them, after asking

Cleaning runs each tool's own command in the project (cargo clean, mix clean, swift package reset, npm run clean,
go clean -cache, brew cleanup, docker system prune, ...). Things no tool can clean (node_modules, .next, __pycache__)
show as "rm -rf" and are only removed with -rm. With -caches, shared tool caches are offered one by one too.

Safety:
  safe      the project's own tooling recreates it exactly (lockfile present, gitignored)
  caution   probably recreatable, but may differ or hold something you want (dist/, no lockfile, not gitignored)
  unsafe    git tracks files in it; never deleted

Flags:
`

type options struct {
	tree, asJSON, caches, yes, dryRun, caution, rm bool
	older                                          time.Duration
	min                                            int64
	kinds                                          map[string]bool
	sortBy                                         string
}

func main() {
	args := os.Args[1:]
	clean := len(args) > 0 && args[0] == "clean"
	if clean {
		args = args[1:]
	}

	fs := flag.NewFlagSet("devsweep", flag.ExitOnError)
	fs.Usage = func() { fmt.Fprint(os.Stderr, usage); fs.PrintDefaults() }
	var o options
	var older, min, kinds string
	fs.BoolVar(&o.tree, "tree", false, "show a tree instead of a table")
	fs.BoolVar(&o.asJSON, "json", false, "print JSON")
	fs.BoolVar(&o.caches, "caches", false, "also list shared tool caches (cargo, go, npm, pnpm, brew, ...)")
	fs.StringVar(&older, "older", "", "only artifacts not built for this long, e.g. 14d, 3w, 2mo, 12h")
	fs.StringVar(&min, "min", "1M", "hide artifacts smaller than this, e.g. 100M, 1G")
	fs.StringVar(&kinds, "kind", "", "only these kinds, comma separated: rust,node,go,python,swift,zig,elixir,cmake")
	fs.StringVar(&o.sortBy, "sort", "size", "sort by size, age or path")
	fs.BoolVar(&o.caution, "include-caution", false, "clean: also offer caution artifacts, each asked one by one")
	fs.BoolVar(&o.rm, "rm", false, "clean: also rm -rf artifacts and caches no tool has a clean command for (node_modules, .next, ...)")
	fs.BoolVar(&o.yes, "yes", false, "clean: don't ask before deleting safe artifacts")
	fs.BoolVar(&o.dryRun, "dry-run", false, "clean: show what would be deleted, delete nothing")
	// Let flags come after the dirs too: devsweep ~/dev -tree
	var roots []string
	for fs.Parse(args); fs.NArg() > 0; fs.Parse(args) {
		roots = append(roots, fs.Arg(0))
		args = fs.Args()[1:]
	}

	var err error
	if o.older, err = parseAge(older); err != nil {
		fail(err)
	}
	if o.min, err = parseSize(min); err != nil {
		fail(err)
	}
	if kinds != "" {
		o.kinds = map[string]bool{}
		for _, k := range strings.Split(kinds, ",") {
			o.kinds[strings.TrimSpace(k)] = true
		}
	}

	if len(roots) == 0 {
		home, _ := os.UserHomeDir()
		roots = []string{filepath.Join(home, "dev")}
	}
	for i, r := range roots {
		abs, err := filepath.Abs(r)
		if err != nil {
			fail(err)
		}
		if fi, err := os.Stat(abs); err != nil || !fi.IsDir() {
			fail(fmt.Errorf("%s: not a directory", r))
		}
		roots[i] = abs
	}

	all, err := scan(roots)
	if err != nil {
		fail(err)
	}
	as := filter(all, o)

	if clean {
		var cs []*Cache
		if o.caches {
			cs = caches()
		}
		os.Exit(runClean(roots, as, cs, o))
	}

	if o.asJSON {
		out := map[string]any{"roots": roots, "artifacts": as}
		if o.caches {
			out["caches"] = caches()
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		enc.Encode(out)
		return
	}

	summary(os.Stdout, roots, as)
	fmt.Println()
	if len(as) == 0 {
		fmt.Println(dim("nothing found"))
	}
	for _, root := range roots {
		in := under(root, as)
		if len(in) == 0 {
			continue
		}
		if o.tree {
			printTree(os.Stdout, root, in)
		} else {
			printList(os.Stdout, root, in)
		}
	}
	if o.caches {
		printCaches(os.Stdout, caches())
	}
}

func filter(all []*Artifact, o options) []*Artifact {
	var as []*Artifact
	for _, a := range all {
		if a.Freed() < o.min && a.Bytes < o.min {
			continue
		}
		if o.older > 0 && time.Since(a.LastBuilt) < o.older {
			continue
		}
		if o.kinds != nil && !o.kinds[a.Kind] {
			continue
		}
		as = append(as, a)
	}
	sort.Slice(as, func(i, j int) bool {
		switch o.sortBy {
		case "age":
			return as[i].LastBuilt.Before(as[j].LastBuilt)
		case "path":
			return as[i].Path < as[j].Path
		default:
			return as[i].Freed() > as[j].Freed()
		}
	})
	return as
}

func under(root string, as []*Artifact) []*Artifact {
	var out []*Artifact
	for _, a := range as {
		if strings.HasPrefix(a.Path, root+string(filepath.Separator)) {
			out = append(out, a)
		}
	}
	return out
}

func parseAge(s string) (time.Duration, error) {
	if s == "" {
		return 0, nil
	}
	units := []struct {
		suffix string
		d      time.Duration
	}{{"mo", 30 * 24 * time.Hour}, {"y", 365 * 24 * time.Hour}, {"w", 7 * 24 * time.Hour}, {"d", 24 * time.Hour}, {"h", time.Hour}}
	for _, u := range units {
		if n, ok := strings.CutSuffix(s, u.suffix); ok {
			v, err := strconv.ParseFloat(n, 64)
			if err != nil {
				break
			}
			return time.Duration(v * float64(u.d)), nil
		}
	}
	return 0, fmt.Errorf("bad -older %q: use e.g. 12h, 14d, 3w, 2mo, 1y", s)
}

func parseSize(s string) (int64, error) {
	s = strings.ToUpper(strings.TrimSuffix(strings.TrimSuffix(strings.ToUpper(s), "B"), "I"))
	mult := int64(1)
	for i, u := range "KMGT" {
		if strings.HasSuffix(s, string(u)) {
			mult = 1 << (10 * (i + 1))
			s = strings.TrimSuffix(s, string(u))
			break
		}
	}
	v, err := strconv.ParseFloat(s, 64)
	if err != nil {
		return 0, fmt.Errorf("bad -min: use e.g. 500K, 100M, 1G")
	}
	return int64(v * float64(mult)), nil
}

func fail(err error) {
	fmt.Fprintln(os.Stderr, "devsweep:", err)
	os.Exit(2)
}
