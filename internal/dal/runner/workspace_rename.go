//go:build linux || darwin

package runner

import (
	"os"
	"path/filepath"
)

func splitParent(name string) (string, string, error) {
	dir, base := filepath.Split(name)
	if base == "" || base == "." || base == ".." {
		return "", "", ErrWorkspaceRequest
	}
	if dir == "" {
		dir = "."
	}
	return dir, base, nil
}

func renameDirFDs(root *os.Root, oldName, newName string, rename func(ofd, nfd *os.File, oldBase, newBase string) error) error {
	oldDir, oldBase, err := splitParent(oldName)
	if err != nil {
		return err
	}
	newDir, newBase, err := splitParent(newName)
	if err != nil {
		return err
	}
	ofd, err := root.Open(oldDir)
	if err != nil {
		return err
	}
	defer func() { _ = ofd.Close() }()
	nfd, err := root.Open(newDir)
	if err != nil {
		return err
	}
	defer func() { _ = nfd.Close() }()
	return rename(ofd, nfd, oldBase, newBase)
}
