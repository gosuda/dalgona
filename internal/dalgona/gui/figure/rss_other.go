//go:build !linux && !darwin && !windows

package figure

func residentBytes(int) (int64, bool) { return 0, false }
