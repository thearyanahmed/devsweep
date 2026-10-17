package main

import (
	"io/fs"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"syscall"
	"time"
)

// Artifact is one directory of build output that some project claims.
type Artifact struct {
	Path      string    `json:"path"`
	Project   string    `json:"project"`
	Kind      string    `json:"kind"`
	Dir       string    `json:"dir"`
	Bytes     int64     `json:"bytes"`        // disk space used
	Shared    int64     `json:"shared_bytes"` // part of Bytes in hard links also used elsewhere (pnpm store): not freed by deleting
	Files     int       `json:"files"`
	LastBuilt time.Time `json:"last_built"`
	Safety    Safety    `json:"-"`
	SafetyStr string    `json:"safety"`
	Why       string    `json:"why"`
	Rebuild   string    `json:"rebuild"`
	Clean     []string  `json:"clean_cmd"` // the tool's own command, run in Project; empty means only rm -rf removes it
}

// Method is how clean removes the artifact, for display.
func (a *Artifact) Method() string {
	if len(a.Clean) == 0 {
		return "rm -rf " + a.Dir
	}
	return strings.Join(a.Clean, " ")
}

// Freed is what deleting the artifact gives back.
func (a *Artifact) Freed() int64 { return a.Bytes - a.Shared }

func scan(roots []string) ([]*Artifact, error) {
	var found []*Artifact
	for _, root := range roots {
		err := filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
			if err != nil {
				return nil // unreadable: skip, don't abort the scan
			}
			if !d.IsDir() || path == root {
				return nil
			}
			name := d.Name()
			if name == ".git" {
				return filepath.SkipDir
			}
			parent := filepath.Dir(path)
			r := match(path, parent, name)
			if r == nil {
				return nil
			}
			cmd, safety, why := r.rebuild(parent)
			a := &Artifact{
				Path:    path,
				Project: parent,
				Kind:    r.kind,
				Dir:     name,
				Safety:  max(r.safety, safety),
				Why:     why,
				Rebuild: cmd,
			}
			if r.clean != nil {
				a.Clean = r.clean(parent, name)
			}
			found = append(found, a)
			return filepath.SkipDir
		})
		if err != nil {
			return nil, err
		}
	}

	var wg sync.WaitGroup
	sem := make(chan struct{}, runtime.NumCPU())
	for _, a := range found {
		wg.Add(1)
		sem <- struct{}{}
		go func() {
			defer func() { <-sem; wg.Done() }()
			measure(a)
			checkGit(a)
			a.SafetyStr = a.Safety.String()
		}()
	}
	wg.Wait()
	return found, nil
}

// measure fills in the size, file count and newest modification time. A hard-linked file counts once, and counts as
// shared only when some of its links live outside the artifact (deleting the artifact doesn't free it then).
func measure(a *Artifact) {
	type inode struct {
		size        int64
		nlink, seen uint64
	}
	links := map[uint64]*inode{}
	filepath.WalkDir(a.Path, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		info, err := d.Info()
		if err != nil {
			return nil
		}
		if t := info.ModTime(); t.After(a.LastBuilt) {
			a.LastBuilt = t
		}
		st, ok := info.Sys().(*syscall.Stat_t)
		if !ok {
			a.Bytes += info.Size()
			return nil
		}
		size := st.Blocks * 512
		if !d.IsDir() && st.Nlink > 1 {
			if n := links[st.Ino]; n != nil {
				n.seen++
				return nil
			}
			links[st.Ino] = &inode{size: size, nlink: uint64(st.Nlink), seen: 1}
		}
		a.Bytes += size
		if !d.IsDir() {
			a.Files++
		}
		return nil
	})
	for _, n := range links {
		if n.seen < n.nlink {
			a.Shared += n.size
		}
	}
}

// checkGit downgrades an artifact whose contents git tracks, and notes one that isn't gitignored.
func checkGit(a *Artifact) {
	if !inGitRepo(a.Project) {
		return
	}
	out, err := exec.Command("git", "-C", a.Project, "ls-files", "--", a.Dir).Output()
	if err != nil {
		return
	}
	if tracked := strings.Count(string(out), "\n"); tracked > 0 {
		a.Safety = Unsafe
		a.Why = plural(tracked, "file") + " tracked in git"
		return
	}
	if exec.Command("git", "-C", a.Project, "check-ignore", "-q", a.Dir).Run() != nil {
		a.Safety = max(a.Safety, Caution)
		a.Why = join(a.Why, "not gitignored")
	}
}

func inGitRepo(dir string) bool {
	for ; ; dir = filepath.Dir(dir) {
		if exists(filepath.Join(dir, ".git")) {
			return true
		}
		if filepath.Dir(dir) == dir {
			return false
		}
	}
}

func join(a, b string) string {
	if a == "" {
		return b
	}
	return a + "; " + b
}
