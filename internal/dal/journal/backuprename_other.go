//go:build !windows

package journal

import "os"

func renameBackupDir(oldpath, newpath string) error {
	return os.Rename(oldpath, newpath)
}
