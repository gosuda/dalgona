//go:build !linux

package client

func dispatchDialProbe() (bool, int) { return false, 0 }
