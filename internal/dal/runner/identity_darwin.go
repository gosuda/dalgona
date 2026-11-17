//go:build darwin

package runner

import (
	"strconv"

	"golang.org/x/sys/unix"
)

func processStartTime(pid int) string {
	info, err := unix.SysctlKinfoProc("kern.proc.pid", pid)
	if err != nil {
		return ""
	}
	start := info.Proc.P_starttime
	return strconv.FormatInt(start.Sec, 10) + ":" + strconv.FormatInt(int64(start.Usec), 10)
}
