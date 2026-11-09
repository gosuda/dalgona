package runner

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

var (
	ErrPinnedSymlink = errors.New("runner: pinned open refused: path contains a symlink")
	ErrPinnedEscape  = errors.New("runner: pinned open refused: path escapes its root")
)

func checkPinnedRel(rel string) (string, error) {
	if strings.IndexByte(rel, 0) >= 0 {
		return "", fmt.Errorf("%w: path contains a NUL byte", ErrPinnedEscape)
	}
	clean := filepath.Clean(rel)
	if clean == "." {
		return clean, nil
	}
	if filepath.IsAbs(clean) || clean == ".." || strings.HasPrefix(clean, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("%w: %s", ErrPinnedEscape, rel)
	}
	return clean, nil
}

func OpenPinned(root, rel string) (*os.File, error) {
	clean, err := checkPinnedRel(rel)
	if err != nil {
		return nil, err
	}
	return openPinned(root, clean)
}

func statNoFollow(path string) (fs.FileInfo, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if info.Mode()&fs.ModeSymlink != 0 {
		return nil, fmt.Errorf("%w: %s", ErrPinnedSymlink, path)
	}
	return info, nil
}
