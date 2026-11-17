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

func walkNoFollow(dir *os.File, rel string) (*os.File, error) {
	parts := strings.Split(filepath.ToSlash(rel), "/")
	cur := dir
	for i, part := range parts {
		last := i == len(parts)-1
		flags := unix.O_RDONLY | unix.O_CLOEXEC | unix.O_NOFOLLOW
		if !last {
			flags |= unix.O_DIRECTORY
		}
		fd, err := unix.Openat(int(cur.Fd()), part, flags, 0)
		if err != nil {
			mapped := classifyPinnedError(cur, part, rel, err)
			_ = cur.Close()
			return nil, mapped
		}
		next := os.NewFile(uintptr(fd), part)
		_ = cur.Close()
		cur = next
	}
	return cur, nil
}

func classifyPinnedError(dir *os.File, name, rel string, err error) error {
	if errors.Is(err, unix.ELOOP) || errors.Is(err, unix.ENOTDIR) {
		var st unix.Stat_t
		if fstatErr := unix.Fstatat(int(dir.Fd()), name, &st, unix.AT_SYMLINK_NOFOLLOW); fstatErr == nil && st.Mode&unix.S_IFMT == unix.S_IFLNK {
			return fmt.Errorf("%w: %s", ErrPinnedSymlink, rel)
		}
	}
	return fmt.Errorf("runner: pinned open %s: %w", rel, err)
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
