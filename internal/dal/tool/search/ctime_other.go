//go:build !linux && !darwin

package search

import "io/fs"

const ctimeSupported = false

func ctimeNS(fs.FileInfo) (int64, bool) {
	return 0, false
}
