//go:build !windows

package search

import (
	"errors"
	"os"
)

func syncDir(path string) error {
	dir, err := os.Open(path)
	if err != nil {
		return err
	}
	syncErr := dir.Sync()
	return errors.Join(syncErr, dir.Close())
}
