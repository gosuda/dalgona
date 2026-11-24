//go:build !linux && !darwin && !windows

package runner

import (
	"errors"
	"io/fs"
	"os"
)

func renameDirNoReplace(root *os.Root, oldName, newName string) error {
	if _, err := root.Lstat(newName); err == nil {
		return ErrWorkspaceExists
	} else if !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	return root.Rename(oldName, newName)
}
