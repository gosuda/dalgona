//go:build !linux && !darwin

package runner

import "io/fs"

const workspaceReadFlag = 0

func nonFileIdentity(_ fs.FileInfo) []byte {
	return nil
}
