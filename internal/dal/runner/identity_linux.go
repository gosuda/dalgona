//go:build linux

package runner

import (
	"os"
	"strconv"
	"strings"
)

func processStartTime(pid int) string {
	data, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/stat")
	if err != nil {
		return ""
	}
	text := string(data)
	_, fieldsText, found := strings.CutLast(text, ")")
	if !found {
		return ""
	}
	fields := strings.Fields(fieldsText)
	if len(fields) <= 19 {
		return ""
	}
	return fields[19]
}
