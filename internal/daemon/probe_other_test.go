//go:build !linux

package daemon

func dispatchDialProbe() (bool, int) { return false, 0 }
