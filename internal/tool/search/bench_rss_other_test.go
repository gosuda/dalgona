//go:build !unix

package search

import "os/exec"

func peakRSSKiB(*exec.Cmd) int64 { return -1 }
