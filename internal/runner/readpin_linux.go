//go:build linux

package runner

import (
	"errors"
	"fmt"
	"os"

	"golang.org/x/sys/unix"
)

func openPinned(root, rel string) (*os.File, error) {
	dir, err := os.Open(root)
	if err != nil {
		return nil, fmt.Errorf("runner: pinned open %s: %w", rel, err)
	}
	file, ok, err := openat2Pinned(dir, rel)
	if ok {
		_ = dir.Close()
		return file, err
	}
	return walkNoFollow(dir, rel)
}

func openat2Pinned(dir *os.File, rel string) (*os.File, bool, error) {
	how := &unix.OpenHow{
		Flags:   unix.O_RDONLY | unix.O_CLOEXEC,
		Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_SYMLINKS,
	}
	fd, err := unix.Openat2(int(dir.Fd()), rel, how)
	if err == nil {
		return os.NewFile(uintptr(fd), rel), true, nil
	}
	if errors.Is(err, unix.ELOOP) {
		return nil, true, fmt.Errorf("%w: %s", ErrPinnedSymlink, rel)
	}
	if errors.Is(err, unix.EXDEV) {
		return nil, true, fmt.Errorf("%w: %s", ErrPinnedEscape, rel)
	}
	return nil, false, nil
}
