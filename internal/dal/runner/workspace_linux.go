package runner

import (
	"encoding/binary"
	"errors"
	"io"
	"io/fs"
	"os"
	"syscall"

	"golang.org/x/sys/unix"
)

const workspaceReadFlag = syscall.O_NONBLOCK

func fileIdentity(info fs.FileInfo, _ func() (io.ReadCloser, error)) []byte {
	st, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		return nil
	}
	identity := binary.BigEndian.AppendUint64(nil, uint64(st.Dev))          //nolint:gosec // the device bits feed a digest
	identity = binary.BigEndian.AppendUint64(identity, uint64(st.Ino))      //nolint:gosec // the inode bits feed a digest
	identity = binary.BigEndian.AppendUint64(identity, uint64(st.Ctim.Sec)) //nolint:gosec // the time bits feed a digest
	return binary.BigEndian.AppendUint64(identity, uint64(st.Ctim.Nsec))    //nolint:gosec // the time bits feed a digest
}

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
