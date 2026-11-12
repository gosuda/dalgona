package ratchet

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path"
	"path/filepath"
	"regexp"
	"strings"

	json "encoding/json/v2"
)

const (
	flowRoot  = ".dalgona/ratchet"
	stateFile = "state.json"

	caseIDPattern = `[A-Za-z0-9][A-Za-z0-9._-]{0,127}`
)

var (
	flowName = regexp.MustCompile(`^[a-z0-9][a-z0-9_-]{0,63}$`)
	caseID   = regexp.MustCompile("^" + caseIDPattern + "$")

	errNoFlow = errors.New("does not exist; call init first")
)

type flow struct {
	root *os.Root
	name string
	dir  string
}

func openFlow(cwd, name string) (*flow, error) {
	if !flowName.MatchString(name) {
		return nil, fmt.Errorf("flow name must match %s: %q", flowName, name)
	}
	root, err := os.OpenRoot(cwd)
	if err != nil {
		return nil, fmt.Errorf("open workspace: %w", err)
	}
	return &flow{root: root, name: name, dir: filepath.Join(".dalgona", "ratchet", name)}, nil
}

func (f *flow) close() error {
	return f.root.Close()
}

func (f *flow) load() (*state, error) {
	data, err := f.root.ReadFile(filepath.Join(f.dir, stateFile))
	if errors.Is(err, fs.ErrNotExist) {
		return nil, fmt.Errorf("flow %q %w", f.name, errNoFlow)
	}
	if err != nil {
		return nil, fmt.Errorf("read flow %q: %w", f.name, err)
	}
	var s state
	if err := json.Unmarshal(data, &s); err != nil {
		return nil, fmt.Errorf("decode flow %q: %w", f.name, err)
	}
	return &s, nil
}

func (f *flow) save(s *state) error {
	data, err := json.Marshal(s, json.Deterministic(true))
	if err != nil {
		return fmt.Errorf("encode flow %q: %w", f.name, err)
	}
	if err := f.root.MkdirAll(f.dir, 0o750); err != nil {
		return fmt.Errorf("create flow %q: %w", f.name, err)
	}
	tmp := filepath.Join(f.dir, stateFile+".tmp")
	if err := f.root.WriteFile(tmp, data, 0o600); err != nil {
		return fmt.Errorf("write flow %q: %w", f.name, err)
	}
	if err := f.root.Rename(tmp, filepath.Join(f.dir, stateFile)); err != nil {
		return fmt.Errorf("commit flow %q: %w", f.name, err)
	}
	return nil
}

func repoPath(p string) (string, error) {
	clean := path.Clean(filepath.ToSlash(p))
	if p == "" || filepath.IsAbs(p) || path.IsAbs(clean) || clean == ".." || strings.HasPrefix(clean, "../") {
		return "", fmt.Errorf("path must be inside the workspace: %s", p)
	}
	if clean == "." {
		return "", fmt.Errorf("path must name a file or directory below the workspace root: %s", p)
	}
	if overlapsFlowRoot(clean) {
		return "", fmt.Errorf("path must stay outside %s: %s", flowRoot, p)
	}
	return clean, nil
}

func overlapsFlowRoot(clean string) bool {
	return clean == flowRoot || strings.HasPrefix(clean, flowRoot+"/") || strings.HasPrefix(flowRoot, clean+"/")
}

func repoPaths(list []string) ([]string, error) {
	out := make([]string, 0, len(list))
	for _, p := range list {
		clean, err := repoPath(p)
		if err != nil {
			return nil, err
		}
		out = append(out, clean)
	}
	return out, nil
}

func (f *flow) workspacePaths(list []string) ([]string, error) {
	out, err := repoPaths(list)
	if err != nil {
		return nil, err
	}
	for _, p := range out {
		_, err := f.root.Stat(filepath.FromSlash(p))
		if errors.Is(err, fs.ErrNotExist) {
			return nil, fmt.Errorf("path does not exist: %s", p)
		}
		if err != nil {
			return nil, fmt.Errorf("stat %s: %w", p, err)
		}
	}
	return out, nil
}

func overlaps(a, b string) bool {
	return a == b || strings.HasPrefix(a, b+"/") || strings.HasPrefix(b, a+"/")
}
