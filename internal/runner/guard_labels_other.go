//go:build !linux

package runner

func processLabels(int) []string { return nil }
