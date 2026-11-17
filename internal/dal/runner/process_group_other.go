//go:build !windows && !darwin && !linux

package runner

func processGroupDrained(pgid int) bool {
	return false
}
