package search

import (
	"io/fs"
	"syscall"
	"time"
)

const ctimeSupported = true

func ctimeNS(info fs.FileInfo) (int64, bool) {
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		return 0, false
	}
	return time.Unix(int64(stat.Ctimespec.Sec), int64(stat.Ctimespec.Nsec)).UnixNano(), true
}
