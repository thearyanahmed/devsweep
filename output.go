package main

import (
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"text/tabwriter"
	"time"
)

var color = func() bool {
	if os.Getenv("NO_COLOR") != "" {
		return false
	}
	fi, err := os.Stdout.Stat()
	return err == nil && fi.Mode()&os.ModeCharDevice != 0
}()

func paint(code, s string) string {
	if !color {
		return s
	}
	return "\x1b[" + code + "m" + s + "\x1b[0m"
}

func dim(s string) string  { return paint("2", s) }
func bold(s string) string { return paint("1", s) }

func safetyLabel(s Safety) string {
	return paint([...]string{"32", "33", "31"}[s], s.String())
}

func human(b int64) string {
	const unit = 1024
	if b < unit {
		return fmt.Sprintf("%d B", b)
	}
	div, exp := int64(unit), 0
	for n := b / unit; n >= unit; n /= unit {
		div *= unit
		exp++
	}
	return fmt.Sprintf("%.1f %ciB", float64(b)/float64(div), "KMGTPE"[exp])
}

func ago(t time.Time) string {
	if t.IsZero() {
		return "-"
	}
	d := time.Since(t)
	switch {
	case d < time.Hour:
		return fmt.Sprintf("%dm ago", int(d.Minutes()))
	case d < 48*time.Hour:
		return fmt.Sprintf("%dh ago", int(d.Hours()))
	case d < 60*24*time.Hour:
		return fmt.Sprintf("%dd ago", int(d.Hours()/24))
	default:
		return fmt.Sprintf("%dmo ago", int(d.Hours()/24/30))
	}
}

func plural(n int, word string) string {
	if n == 1 {
		return "1 " + word
	}
	return fmt.Sprintf("%d %ss", n, word)
}

// tilde shortens a path under $HOME for display.
func tilde(p string) string {
	home, _ := os.UserHomeDir()
	if home != "" && (p == home || strings.HasPrefix(p, home+"/")) {
		return "~" + p[len(home):]
	}
	return p
}

func rel(root, p string) string {
	if r, err := filepath.Rel(root, p); err == nil && !strings.HasPrefix(r, "..") {
		return r
	}
	return tilde(p)
}

func summary(w io.Writer, roots []string, as []*Artifact) {
	var by [3]int64
	projects := map[string]bool{}
	for _, a := range as {
		by[a.Safety] += a.Freed()
		projects[a.Project] = true
	}
	names := make([]string, len(roots))
	for i, r := range roots {
		names[i] = tilde(r)
	}
	fmt.Fprintf(w, "%s  %s in %s, %s  %s  %s\n",
		bold(strings.Join(names, ", ")),
		plural(len(as), "artifact"), plural(len(projects), "project"),
		paint("32", human(by[Safe])+" safe"),
		paint("33", human(by[Caution])+" caution"),
		paint("31", human(by[Unsafe])+" unsafe"))
}

func printList(w io.Writer, root string, as []*Artifact) {
	tw := tabwriter.NewWriter(w, 0, 0, 2, ' ', 0)
	fmt.Fprintln(tw, dim("SIZE\tSAFETY\tKIND\tPROJECT\tDIR\tLAST BUILT\tCLEAN WITH\tREBUILD WITH"))
	for _, a := range as {
		note := a.Rebuild
		if a.Why != "" {
			note += dim("  (" + a.Why + ")")
		}
		size := human(a.Freed())
		if a.Shared > 0 {
			size += dim(" +" + human(a.Shared) + " shared")
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n",
			size, safetyLabel(a.Safety), a.Kind, rel(root, a.Project), a.Dir, ago(a.LastBuilt), method(a), note)
	}
	tw.Flush()
}

// node is a directory in the tree view; leaves are artifacts.
type node struct {
	name     string
	bytes    int64
	children map[string]*node
	art      *Artifact
}

func printTree(w io.Writer, root string, as []*Artifact) {
	top := &node{name: tilde(root), children: map[string]*node{}}
	for _, a := range as {
		n := top
		n.bytes += a.Freed()
		for _, part := range strings.Split(rel(root, a.Path), string(filepath.Separator)) {
			c := n.children[part]
			if c == nil {
				c = &node{name: part, children: map[string]*node{}}
				n.children[part] = c
			}
			c.bytes += a.Freed()
			n = c
		}
		n.art = a
	}
	fmt.Fprintf(w, "%s  %s\n", bold(top.name), human(top.bytes))
	walkTree(w, top, "")
}

func walkTree(w io.Writer, n *node, prefix string) {
	kids := make([]*node, 0, len(n.children))
	for _, c := range n.children {
		kids = append(kids, c)
	}
	sort.Slice(kids, func(i, j int) bool { return kids[i].bytes > kids[j].bytes })
	for i, c := range kids {
		branch, next := "├── ", "│   "
		if i == len(kids)-1 {
			branch, next = "└── ", "    "
		}
		// Fold single-child chains (a/b/c) into one line, the way most tree views do.
		name := c.name
		for c.art == nil && len(c.children) == 1 {
			for _, only := range c.children {
				name += "/" + only.name
				c = only
			}
		}
		if a := c.art; a != nil {
			note := "clean: " + method(a) + ", rebuild: " + a.Rebuild
			if a.Why != "" {
				note += " (" + a.Why + ")"
			}
			fmt.Fprintf(w, "%s%s%s  %s  %s  %s  %s\n", prefix, branch, paint("36", name), human(c.bytes),
				safetyLabel(a.Safety), dim(a.Kind+", built "+ago(a.LastBuilt)), dim(note))
			continue
		}
		fmt.Fprintf(w, "%s%s%s  %s\n", prefix, branch, name, dim(human(c.bytes)))
		walkTree(w, c, prefix+next)
	}
}

func printCaches(w io.Writer, cs []*Cache) {
	sort.Slice(cs, func(i, j int) bool { return cs[i].Bytes > cs[j].Bytes })
	var total int64
	for _, c := range cs {
		total += c.Bytes
	}
	fmt.Fprintf(w, "\n%s  %s  %s\n", bold("Shared caches"), human(total), dim("(refilled on demand; devsweep clean -caches offers each one)"))
	tw := tabwriter.NewWriter(w, 0, 0, 2, ' ', 0)
	fmt.Fprintln(tw, dim("SIZE\tSAFETY\tCACHE\tPATH\tCLEAN WITH"))
	for _, c := range cs {
		path := tilde(c.Path)
		if path == "" {
			path = "-"
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\n", human(c.Bytes), safetyLabel(c.Safety), c.Name, path, dim(c.Method()+notes(c.Note)))
	}
	tw.Flush()
}

// method shows how clean removes an artifact; rm -rf stands out since it needs -rm.
func method(a *Artifact) string {
	if len(a.Clean) == 0 {
		return paint("35", a.Method())
	}
	return a.Method()
}

func notes(n string) string {
	if n == "" {
		return ""
	}
	return "  (" + n + ")"
}
