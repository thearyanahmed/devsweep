package main

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"
)

// Cache is a shared, tool-wide cache outside any project.
type Cache struct {
	Name   string   `json:"name"`
	Path   string   `json:"path"`
	Bytes  int64    `json:"bytes"`
	Cmd    []string `json:"clean_cmd"` // the tool's own command for trimming it; empty means only rm -rf Path
	Safety Safety   `json:"-"`
	Note   string   `json:"note,omitempty"`
	size   func() int64
}

func (c *Cache) Method() string {
	if len(c.Cmd) == 0 {
		return "rm -rf " + tilde(c.Path)
	}
	return strings.Join(c.Cmd, " ")
}

func caches() []*Cache {
	home, _ := os.UserHomeDir()
	h := func(p string) string { return filepath.Join(home, p) }
	list := []*Cache{
		{Name: "cargo registry", Path: h(".cargo/registry/cache"), Note: "cargo also trims it on its own"},
		{Name: "cargo registry src", Path: h(".cargo/registry/src"), Note: "cargo also trims it on its own"},
		{Name: "cargo git", Path: h(".cargo/git/checkouts")},
		{Name: "go build cache", Path: goenv("GOCACHE", h("Library/Caches/go-build")), Cmd: []string{"go", "clean", "-cache"}},
		{Name: "go module cache", Path: goenv("GOMODCACHE", h("go/pkg/mod")), Cmd: []string{"go", "clean", "-modcache"}},
		{Name: "npm cache", Path: h(".npm"), Cmd: []string{"npm", "cache", "clean", "--force"}},
		{Name: "pnpm store", Path: pnpmStore(h("Library/pnpm/store")), Cmd: []string{"pnpm", "store", "prune"}, Note: "keeps packages a project still uses; prunes the store of the pnpm on PATH"},
		{Name: "yarn cache", Path: h("Library/Caches/Yarn"), Cmd: []string{"yarn", "cache", "clean"}},
		{Name: "bun cache", Path: h(".bun/install/cache"), Cmd: []string{"bun", "pm", "cache", "rm"}},
		{Name: "corepack", Path: h(".cache/node/corepack"), Cmd: []string{"corepack", "cache", "clean"}},
		{Name: "pip cache", Path: h("Library/Caches/pip"), Cmd: []string{"pip3", "cache", "purge"}},
		{Name: "uv cache", Path: h(".cache/uv"), Cmd: []string{"uv", "cache", "prune"}},
		{Name: "homebrew", Path: h("Library/Caches/Homebrew"), Cmd: []string{"brew", "cleanup", "--prune=all"}},
		{Name: "xcode derived data", Path: h("Library/Developer/Xcode/DerivedData")},
		{Name: "ios simulators (unavailable)", Path: h("Library/Developer/CoreSimulator/Devices"), Cmd: []string{"xcrun", "simctl", "delete", "unavailable"}, Note: "only removes simulators for runtimes no longer installed"},
		{Name: "electron", Path: h("Library/Caches/electron")},
		{Name: "electron-builder", Path: h("Library/Caches/electron-builder")},
		{Name: "docker (reclaimable)", Cmd: []string{"docker", "system", "prune", "-f"}, Safety: Caution, size: dockerReclaimable,
			Note: "stopped containers, unused networks, dangling images, build cache; add -a for all unused images"},
	}
	var out []*Cache
	var wg sync.WaitGroup
	for _, c := range list {
		if c.size == nil && !exists(c.Path) {
			continue
		}
		if c.size != nil && len(c.Cmd) > 0 {
			if _, err := exec.LookPath(c.Cmd[0]); err != nil {
				continue
			}
		}
		out = append(out, c)
		wg.Add(1)
		go func() {
			defer wg.Done()
			if c.size != nil {
				c.Bytes = c.size()
				return
			}
			a := &Artifact{Path: c.Path}
			measure(a)
			c.Bytes = a.Bytes
		}()
	}
	wg.Wait()
	// Docker not running, or nothing to reclaim: leave it out.
	kept := out[:0]
	for _, c := range out {
		if c.Bytes != 0 {
			kept = append(kept, c)
		}
	}
	return kept
}

func goenv(v, def string) string {
	if out, err := exec.Command("go", "env", v).Output(); err == nil && len(strings.TrimSpace(string(out))) > 0 {
		return strings.TrimSpace(string(out))
	}
	return def
}

func pnpmStore(def string) string {
	// `pnpm store path` names one store version (…/store/v10); measure every version beside it.
	if out, err := exec.Command("pnpm", "store", "path").Output(); err == nil && len(strings.TrimSpace(string(out))) > 0 {
		return filepath.Dir(strings.TrimSpace(string(out)))
	}
	return def
}

var dockerSize = regexp.MustCompile(`^([\d.]+)\s*([kKMGT]?B)`)

// dockerReclaimable adds up what `docker system df` says could be reclaimed; 0 when Docker isn't running.
func dockerReclaimable() int64 {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, "docker", "system", "df", "--format", "{{.Reclaimable}}").Output()
	if err != nil {
		return 0
	}
	mult := map[string]float64{"B": 1, "kB": 1e3, "KB": 1e3, "MB": 1e6, "GB": 1e9, "TB": 1e12}
	var total int64
	for _, line := range strings.Split(strings.TrimSpace(string(out)), "\n") {
		m := dockerSize.FindStringSubmatch(strings.TrimSpace(line))
		if m == nil {
			continue
		}
		v, _ := strconv.ParseFloat(m[1], 64)
		total += int64(v * mult[m[2]])
	}
	return total
}
