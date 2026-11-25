package runner

import (
	"errors"
	"os"
	"syscall"

	"golang.org/x/sys/unix"
)

const workspaceReadFlag = syscall.O_NONBLOCK

func renameDirNoReplace(root *os.Root, oldName, newName string) error {
	return renameDirFDs(root, oldName, newName, func(ofd, nfd *os.File, oldBase, newBase string) error {
		if err := unix.Renameat2(int(ofd.Fd()), oldBase, int(nfd.Fd()), newBase, unix.RENAME_NOREPLACE); err != nil { //nolint:gosec // descriptors come from root-confined opens
			if errors.Is(err, unix.EEXIST) {
				return ErrWorkspaceExists
			}
			return err
		}
		return nil
	})
}
