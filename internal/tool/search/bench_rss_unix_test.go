//go:build unix

package search

import (
	"os/exec"
	"runtime"
	"syscall"
)

func peakRSSKiB(cmd *exec.Cmd) int64 {
	usage, ok := cmd.ProcessState.SysUsage().(*syscall.Rusage)
	if !ok {
		return -1
	}
	if runtime.GOOS == "darwin" {
		return int64(usage.Maxrss) / 1024
	}
	return int64(usage.Maxrss)
}
