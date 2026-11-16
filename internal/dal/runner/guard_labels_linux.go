//go:build linux

package runner

import (
	"os"
	"strconv"
	"strings"
)

func processLabels(pid int) []string {
	if pid <= 0 {
		return nil
	}
	base := "/proc/" + strconv.Itoa(pid)
	var labels []string
	if comm, err := os.ReadFile(base + "/comm"); err == nil {
		labels = append(labels, strings.TrimSpace(string(comm)))
	}
	if cmdline, err := os.ReadFile(base + "/cmdline"); err == nil {
		labels = append(labels, strings.ReplaceAll(strings.TrimRight(string(cmdline), "\x00"), "\x00", " "))
	}
	return labels
}
