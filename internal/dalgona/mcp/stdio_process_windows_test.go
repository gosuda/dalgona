//go:build windows

package mcp

import (
	"math"
	"strconv"
	"testing"
)

func TestWindowsPIDBoundaries(t *testing.T) {
	if strconv.IntSize != 64 {
		t.Skip("boundary values need 64-bit int")
	}
	cases := []struct {
		name string
		pid  int
		want uint32
		ok   bool
	}{
		{"negative", -1, 0, false},
		{"zero", 0, 0, true},
		{"max", int(math.MaxUint32), math.MaxUint32, true},
		{"over", int(uint64(math.MaxUint32) + 1), 0, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, ok := windowsPID(tc.pid)
			if ok != tc.ok || got != tc.want {
				t.Fatalf("windowsPID(%d) = (%d, %v), want (%d, %v)", tc.pid, got, ok, tc.want, tc.ok)
			}
		})
	}
}
