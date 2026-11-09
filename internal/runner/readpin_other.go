//go:build !unix

package runner

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

func openPinned(root, rel string) (*os.File, error) {
	cur := root
	parts := strings.Split(filepath.ToSlash(rel), "/")
	for _, part := range parts[:len(parts)-1] {
		cur = filepath.Join(cur, part)
		info, err := os.Lstat(cur)
		if err != nil {
			return nil, err
		}
		if info.Mode()&fs.ModeSymlink != 0 {
			return nil, fmt.Errorf("%w: %s", ErrPinnedSymlink, rel)
		}
		if !info.IsDir() {
			return nil, fmt.Errorf("%w: %s", ErrPinnedEscape, rel)
		}
	}
	leaf := filepath.Join(cur, parts[len(parts)-1])
	info, err := os.Lstat(leaf)
	if err != nil {
		return nil, err
	}
	if info.Mode()&fs.ModeSymlink != 0 {
		return nil, fmt.Errorf("%w: %s", ErrPinnedSymlink, rel)
	}
	return os.Open(leaf)
}

func openNoFollow(path string) (*os.File, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if info.Mode()&fs.ModeSymlink != 0 {
		return nil, fmt.Errorf("%w: %s", ErrPinnedSymlink, path)
	}
	return os.Open(path)
}
