//go:build windows

package figure

import (
	"math"
	"unsafe"

	"golang.org/x/sys/windows"
)

var processMemoryInfo = windows.NewLazySystemDLL("psapi.dll").NewProc("GetProcessMemoryInfo")

type processMemoryCounters struct {
	header [2]uint32
	sizes  [8]uintptr
}

const workingSetSize = 1

func residentBytes(pid int) (int64, bool) {
	if pid < 0 || pid > math.MaxUint32 {
		return 0, false
	}
	process, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION, false, uint32(pid))
	if err != nil {
		return 0, false
	}
	defer func() { _ = windows.CloseHandle(process) }()
	var counters processMemoryCounters
	counters.header[0] = uint32(unsafe.Sizeof(counters))
	ok, _, _ := processMemoryInfo.Call(uintptr(process), uintptr(unsafe.Pointer(&counters)), unsafe.Sizeof(counters)) //nolint:gosec // G103: psapi requires the counters struct address and the size argument bounds the write
	if ok == 0 {
		return 0, false
	}
	usage := uint64(counters.sizes[workingSetSize])
	if usage > math.MaxInt64 {
		return 0, false
	}
	return int64(usage), true
}
