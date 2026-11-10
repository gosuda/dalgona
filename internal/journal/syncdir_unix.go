//go:build !windows

package journal

import (
	"errors"
	"os"
)

func syncDir(dir string) (err error) {
	file, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, file.Close()) }()
	return file.Sync()
}
