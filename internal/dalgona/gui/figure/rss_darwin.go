//go:build darwin

package figure

import (
	"encoding/binary"
	"math"
	"unsafe"

	"golang.org/x/sys/unix"
)

const (
	procInfoCallPIDInfo = 2
	procPIDTaskInfo     = 4
	taskInfoBytes       = 6*8 + 12*4
	residentSizeOffset  = 8
)

func residentBytes(pid int) (int64, bool) {
	var info [taskInfoBytes]byte
	//nolint:gosec,staticcheck // proc_pidinfo writes taskInfoBytes into the resident stack array; x/sys/unix ships no libSystem wrapper for it
	written, _, errno := unix.Syscall6(unix.SYS_PROC_INFO, procInfoCallPIDInfo, uintptr(pid), procPIDTaskInfo, 0, uintptr(unsafe.Pointer(&info[0])), taskInfoBytes)
	if errno != 0 || written != taskInfoBytes {
		return 0, false
	}
	resident := binary.LittleEndian.Uint64(info[residentSizeOffset:])
	if resident > math.MaxInt64 {
		return 0, false
	}
	return int64(resident), true
}
