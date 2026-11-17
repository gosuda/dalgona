//go:build !linux

package journal

func syncfsDir(string) error { return nil }
