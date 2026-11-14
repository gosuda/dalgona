//go:build linux

package journal

import (
	"errors"
	"os"

	"golang.org/x/sys/unix"
)

func syncfsDir(path string) (retErr error) {
	d, err := os.Open(path) //nolint:gosec // root path is validated
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, d.Close()) }()
	return unix.Syncfs(int(d.Fd()))
}
