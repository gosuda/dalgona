//go:build !windows && !darwin

package runner

func processGroupDrained(pgid int) bool {
	return false
}
