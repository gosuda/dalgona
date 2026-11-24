//go:build unix

package runner

import (
	"golang.org/x/sys/unix"
)

func makeFifo(path string) error {
	return unix.Mkfifo(path, 0o644)
}
