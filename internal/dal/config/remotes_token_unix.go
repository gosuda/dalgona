//go:build unix

package config

import (
	"errors"
	"os"

	"golang.org/x/sys/unix"
)

func openTokenFile(path string) (*os.File, error) {
	file, err := os.OpenFile(path, os.O_RDONLY|unix.O_NOFOLLOW|unix.O_NONBLOCK, 0)
	if errors.Is(err, unix.ELOOP) || errors.Is(err, unix.ENXIO) || errors.Is(err, unix.ENODEV) {
		return nil, errTokenNotRegular
	}
	return file, err
}
