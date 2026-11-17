//go:build !windows

package patch

import "os"

func replaceInRoot(root *os.Root, oldName, newName string) error {
	return root.Rename(oldName, newName)
}
