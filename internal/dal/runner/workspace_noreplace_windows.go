//go:build windows

package runner

import (
	"errors"
	"os"
	"path/filepath"

	"golang.org/x/sys/windows"
)

func renameDirNoReplace(root *os.Root, oldName, newName string) error {
	oldFull, err := windows.UTF16PtrFromString(filepath.Join(root.Name(), oldName))
	if err != nil {
		return err
	}
	newFull, err := windows.UTF16PtrFromString(filepath.Join(root.Name(), newName))
	if err != nil {
		return err
	}
	if err := windows.MoveFile(oldFull, newFull); err != nil {
		var errno windows.Errno
		if errors.As(err, &errno) && (errno == windows.ERROR_FILE_EXISTS || errno == windows.ERROR_ALREADY_EXISTS) {
			return ErrWorkspaceExists
		}
		return err
	}
	return nil
}
