package client

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sync"
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
	build, err := hashFile(abs)
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

var (
	buildOnce  sync.Once
	buildValue string
)

func Build() string {
	buildOnce.Do(func() {
		if own, err := CurrentBackend(); err == nil {
			buildValue = own.Build
		}
	})
	return buildValue
}

func hashFile(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	digest := sha256.New()
	if _, err := io.Copy(digest, file); err != nil {
		return "", err
	}
	return hex.EncodeToString(digest.Sum(nil))[:16], nil
}
