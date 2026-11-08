//go:build !linux && !darwin && !windows

package runner

func processStartTime(pid int) string { return "" }
