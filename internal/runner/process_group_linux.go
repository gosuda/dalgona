//go:build linux

package runner

import (
	"os"
	"strconv"
	"strings"
)

func processGroupDrained(pgid int) bool {
	entries, err := os.ReadDir("/proc")
	if err != nil {
		return false
	}
	for _, entry := range entries {
		pid, err := strconv.Atoi(entry.Name())
		if err != nil {
			continue
		}
		if memberRunsGroup(pid, pgid) {
			return false
		}
	}
	return true
}

func memberRunsGroup(pid int, pgid int) bool {
	data, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/stat")
	if err != nil {
		return false
	}
	_, fieldsText, found := strings.CutLast(string(data), ")")
	if !found {
		return false
	}
	fields := strings.Fields(fieldsText)
	if len(fields) < 3 {
		return false
	}
	group, err := strconv.Atoi(fields[2])
	if err != nil || group != pgid {
		return false
	}
	return fields[0] != "Z"
}
