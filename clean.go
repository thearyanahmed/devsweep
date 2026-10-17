package main

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
	"strings"
)

// A job is one thing clean will do: a tool command run in a directory, or an rm -rf.
type job struct {
	label  string
	dir    string   // where the command runs
	argv   []string // empty: rm -rf paths
	paths  []string // what it should free; measured before and after
	before int64
}

func (j *job) method() string {
	if len(j.argv) == 0 {
		return "rm -rf"
	}
	return strings.Join(j.argv, " ")
}

func runClean(roots []string, as []*Artifact, cs []*Cache, o options) int {
	root := roots[0]
	in := bufio.NewReader(os.Stdin)
	var batch, oneByOne []*job
	var needRM, caution, unsafe []*Artifact

	// Several artifacts of one project can share a command (cargo clean in a workspace): run it once.
	byCmd := map[string]*job{}
	add := func(a *Artifact, list *[]*job) {
		if len(a.Clean) == 0 {
			j := &job{label: rel(root, a.Path), dir: a.Project, paths: []string{a.Path}, before: a.Bytes}
			*list = append(*list, j)
			return
		}
		key := a.Project + "\x00" + strings.Join(a.Clean, "\x00")
		if j := byCmd[key]; j != nil {
			j.paths = append(j.paths, a.Path)
			j.before += a.Bytes
			return
		}
		j := &job{label: rel(root, a.Path), dir: a.Project, argv: a.Clean, paths: []string{a.Path}, before: a.Bytes}
		byCmd[key] = j
		*list = append(*list, j)
	}
	for _, a := range as {
		switch {
		case a.Safety == Unsafe:
			unsafe = append(unsafe, a)
		case len(a.Clean) == 0 && !o.rm:
			needRM = append(needRM, a)
		case a.Safety == Caution && !o.caution:
			caution = append(caution, a)
		case a.Safety == Caution:
			add(a, &oneByOne)
		default:
			add(a, &batch)
		}
	}

	var freed int64
	failed := 0
	do := func(j *job) {
		if o.dryRun {
			where := ""
			if j.dir != "" && len(j.argv) > 0 {
				where = dim("(cd "+tilde(j.dir)+")") + "  "
			}
			fmt.Printf("  would run  %s%s\n", where, j.method()+rmTarget(j))
			freed += j.before
			return
		}
		if err := j.run(); err != nil {
			fmt.Fprintf(os.Stderr, "  %s %s: %v\n", paint("31", "failed"), j.label, err)
			failed++
		}
		after := sizeOf(j.paths)
		freed += j.before - after
		fmt.Printf("  %s  %s  freed %s\n", paint("32", "done"), j.label, human(j.before-after))
	}

	if len(batch) > 0 {
		var total int64
		for _, j := range batch {
			total += j.before
		}
		fmt.Printf("%s  %s, %s\n\n", paint("32", "Safe to clean"), plural(len(batch), "job"), human(total))
		printJobs(batch)
		if o.dryRun || o.yes || ask(in, fmt.Sprintf("\nRun these %d (%s)?", len(batch), human(total))) {
			for _, j := range batch {
				do(j)
			}
		}
	}
	if len(oneByOne) > 0 {
		fmt.Printf("\n%s  asked one by one\n", paint("33", "Caution"))
		for _, j := range oneByOne {
			if o.dryRun || ask(in, fmt.Sprintf("%s  %s (%s)?", j.method()+rmTarget(j), j.label, human(j.before))) {
				do(j)
			}
		}
	}

	if len(cs) > 0 {
		fmt.Printf("\n%s  asked one by one\n", bold("Shared caches"))
		for _, c := range cs {
			if len(c.Cmd) == 0 && !o.rm {
				fmt.Printf("  %s  %s  %s\n", dim("skipped"), c.Name, dim("(no clean command; -rm to rm -rf "+tilde(c.Path)+")"))
				continue
			}
			j := &job{label: c.Name, argv: c.Cmd, before: c.Bytes}
			if c.Path != "" {
				j.paths = []string{c.Path}
			}
			if len(j.argv) == 0 {
				j.dir = c.Path
			}
			note := ""
			if c.Note != "" {
				note = dim("  " + c.Note)
			}
			q := fmt.Sprintf("%s  %s %s?%s", c.Name, human(c.Bytes), dim("via "+c.Method()), note)
			if o.dryRun || ask(in, q) {
				do(j)
			}
		}
	}

	if len(needRM) > 0 {
		var total int64
		for _, a := range needRM {
			total += a.Freed()
		}
		fmt.Printf("\n%s  %s (%s) no tool cleans; add -rm to rm -rf:\n",
			dim("Left alone"), plural(len(needRM), "artifact"), human(total))
		for _, a := range needRM {
			fmt.Printf("  %s  %s\n", human(a.Freed()), dim(rel(root, a.Path)))
		}
	}
	if len(caution) > 0 {
		fmt.Printf("\n%s  %s left alone; add -include-caution to be asked about each\n",
			paint("33", "Caution"), plural(len(caution), "artifact"))
	}
	if len(unsafe) > 0 {
		fmt.Printf("\n%s  %s tracked in git, never cleaned\n", paint("31", "Unsafe"), plural(len(unsafe), "artifact"))
	}

	verb := "Freed"
	if o.dryRun {
		verb = "Would free"
	}
	fmt.Printf("\n%s %s\n", verb, bold(human(freed)))
	if failed > 0 {
		return 1
	}
	return 0
}

func rmTarget(j *job) string {
	if len(j.argv) > 0 {
		return ""
	}
	parts := make([]string, len(j.paths))
	for i, p := range j.paths {
		parts[i] = tilde(p)
	}
	return " " + strings.Join(parts, " ")
}

func (j *job) run() error {
	if len(j.argv) == 0 {
		for _, p := range j.paths {
			if err := os.RemoveAll(p); err != nil {
				return err
			}
		}
		return nil
	}
	if _, err := exec.LookPath(j.argv[0]); err != nil {
		return fmt.Errorf("%s isn't installed", j.argv[0])
	}
	fmt.Printf("  %s %s\n", dim("$"), dim(j.method()))
	cmd := exec.Command(j.argv[0], j.argv[1:]...)
	cmd.Dir = j.dir
	cmd.Stdin, cmd.Stdout, cmd.Stderr = os.Stdin, os.Stdout, os.Stderr
	return cmd.Run()
}

func printJobs(js []*job) {
	for _, j := range js {
		fmt.Printf("  %-10s  %s  %s\n", human(j.before), j.label, dim("via "+j.method()+rmTarget(j)))
	}
}

func sizeOf(paths []string) int64 {
	var n int64
	for _, p := range paths {
		if !exists(p) {
			continue
		}
		a := &Artifact{Path: p}
		measure(a)
		n += a.Bytes
	}
	return n
}

func ask(in *bufio.Reader, q string) bool {
	fmt.Printf("%s [y/N] ", q)
	line, _ := in.ReadString('\n')
	line = strings.ToLower(strings.TrimSpace(line))
	return line == "y" || line == "yes"
}
