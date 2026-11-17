//go:build unix

package github

import (
	"errors"
	"fmt"
	"io"
	"os"

	"golang.org/x/sys/unix"
)

const maxPrivateKeyBytes = 65536

var errPrivateKeyNotPrivate = errors.New("github: private key file must be private to the current user")

func readPrivateKeyFile(path string) ([]byte, error) {
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0)
	if err != nil {
		return nil, fmt.Errorf("github: open private key: %w", err)
	}
	file := os.NewFile(uintptr(fd), path)
	if file == nil {
		_ = unix.Close(fd)
		return nil, errors.New("github: open private key: invalid file descriptor")
	}
	defer func() { _ = file.Close() }()

	var st unix.Stat_t
	if fstatErr := unix.Fstat(int(file.Fd()), &st); fstatErr != nil {
		return nil, fmt.Errorf("github: stat private key: %w", fstatErr)
	}
	if st.Mode&unix.S_IFMT != unix.S_IFREG {
		return nil, errPrivateKeyNotPrivate
	}
	if int(st.Uid) != os.Geteuid() {
		return nil, errPrivateKeyNotPrivate
	}
	if st.Mode&0o077 != 0 {
		return nil, errPrivateKeyNotPrivate
	}

	data, err := io.ReadAll(io.LimitReader(file, maxPrivateKeyBytes+1))
	if err != nil {
		return nil, fmt.Errorf("github: read private key: %w", err)
	}
	if len(data) > maxPrivateKeyBytes {
		return nil, errors.New("github: private key file exceeds 65536 bytes")
	}
	return data, nil
}
