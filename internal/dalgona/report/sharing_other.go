//go:build !windows

package report

func isSharingViolation(error) bool {
	return false
}
