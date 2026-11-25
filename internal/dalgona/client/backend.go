package client

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/gosuda/dalgona/internal/dal/config"
)

type Backend struct {
	Path  string
	Build string
}

func ResolveBackend(path string) (Backend, error) {
	if resolved, err := filepath.EvalSymlinks(path); err == nil {
		path = resolved
	}
	abs, err := filepath.Abs(path)
	if err != nil {
		return Backend{}, fmt.Errorf("backend %s: %w", path, err)
	}
	build, err := config.FileBuild(abs)
	if err != nil {
		return Backend{}, fmt.Errorf("backend %s: %w", abs, err)
	}
	return Backend{Path: abs, Build: build}, nil
}

func CurrentBackend() (Backend, error) {
	exe, err := os.Executable()
	if err != nil {
		return Backend{}, fmt.Errorf("locate executable: %w", err)
	}
	return ResolveBackend(exe)
}
