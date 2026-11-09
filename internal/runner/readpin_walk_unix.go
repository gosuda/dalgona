//go:build unix

package runner

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/sys/unix"
)

func splitPinnedRel(rel string) []string {
	return strings.Split(filepath.ToSlash(rel), "/")
}

func walkNoFollow(dir *os.File, rel string) (*os.File, error) {
	parts := splitPinnedRel(rel)
	cur := dir
	for i, part := range parts {
		last := i == len(parts)-1
		flags := unix.O_RDONLY | unix.O_CLOEXEC | unix.O_NOFOLLOW
		if !last {
			flags |= unix.O_DIRECTORY
		}
		fd, err := unix.Openat(int(cur.Fd()), part, flags, 0)
		if err != nil {
			_ = cur.Close()
			if errors.Is(err, unix.ELOOP) {
				return nil, fmt.Errorf("%w: %s", ErrPinnedSymlink, rel)
			}
			return nil, fmt.Errorf("runner: pinned open %s: %w", rel, err)
		}
		next := os.NewFile(uintptr(fd), part)
		_ = cur.Close()
		cur = next
	}
	return cur, nil
}

func openNoFollow(path string) (*os.File, error) {
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0)
	if err != nil {
		if errors.Is(err, unix.ELOOP) {
			return nil, fmt.Errorf("%w: %s", ErrPinnedSymlink, path)
		}
		return nil, err
	}
	return os.NewFile(uintptr(fd), path), nil
}
