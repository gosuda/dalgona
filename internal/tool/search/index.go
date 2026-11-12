package search

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
	searchDir     = "search"
	indexHashSize = 8
)

type indexRef struct {
	root string
	gen  string
}

func indexRoot(home, cwd string) (string, error) {
	resolvedHome := home
	if real, err := filepath.EvalSymlinks(home); err == nil {
		resolvedHome = real
	}
	workspace, err := filepath.EvalSymlinks(cwd)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256([]byte(workspace))
	return filepath.Join(resolvedHome, searchDir, hex.EncodeToString(sum[:indexHashSize])), nil
}

func indexDir(env tool.Env) (string, bool) {
	if !tool.IsLocalRunner(env.Runner) || !filepath.IsAbs(env.Home) || !ctimeSupported || runner.UnavailableCause() != "" {
		return "", false
	}
	if env.Sandbox.Mode != "" && env.Sandbox.Mode != runner.SandboxWorkspaceWrite {
		return "", false
	}
	root, err := indexRoot(env.Home, env.Cwd)
	if err != nil || insideWritable(root, env) {
		return "", false
	}
	return root, true
}

func insideWritable(root string, env tool.Env) bool {
	writable := []string{env.Cwd, os.TempDir()}
	if cache, err := os.UserCacheDir(); err == nil {
		writable = append(writable, cache)
	}
	writable = append(writable, env.Sandbox.WritableRoots...)
	for _, dir := range writable {
		if !escapesRoot(resolvedDir(dir), root) {
			return true
		}
	}
	return false
}

func resolvedDir(dir string) string {
	if real, err := filepath.EvalSymlinks(dir); err == nil {
		return real
	}
	return dir
}

func indexFor(env tool.Env) indexRef {
	root, ok := indexDir(env)
	if !ok {
		return indexRef{}
	}
	return indexRef{root: root, gen: currentGeneration(root)}
}

func currentGeneration(root string) string {
	data, err := os.ReadFile(filepath.Join(root, currentFile))
	if err != nil {
		return ""
	}
	gen := strings.TrimSpace(string(data))
	if !singlePathElement(gen) {
		return ""
	}
	info, err := os.Stat(filepath.Join(root, gen))
	if err != nil || !info.IsDir() {
		return ""
	}
	return gen
}

func forgetGeneration(root, gen string) error {
	path := filepath.Join(root, currentFile)
	data, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if strings.TrimSpace(string(data)) != gen {
		return nil
	}
	if err = os.Remove(path); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	return nil
}
