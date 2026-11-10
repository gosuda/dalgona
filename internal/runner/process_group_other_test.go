//go:build !windows && !darwin

package runner

import (
	"testing"
)

func TestProcessGroupDrainedReportsFalse(t *testing.T) {
	if processGroupDrained(1) {
		t.Fatal("processGroupDrained(1) = true, want false where the kernel reports zombies itself")
	}
	if processGroupDrained(1 << 30) {
		t.Fatal("processGroupDrained missing group = true, want false where the kernel reports zombies itself")
	}
}
