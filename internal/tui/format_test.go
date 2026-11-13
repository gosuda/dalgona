package tui

import (
	"testing"
	"time"
)

func TestFormatElapsed(t *testing.T) {
	t.Parallel()
	cases := []struct {
		d    time.Duration
		want string
	}{
		{0, "0 ms"},
		{999 * time.Millisecond, "999 ms"},
		{time.Second, "1s"},
		{59999 * time.Millisecond, "59s"},
		{time.Minute, "1m 00s"},
		{3599999 * time.Millisecond, "59m 59s"},
		{time.Hour, "1h 00m"},
	}
	for _, tc := range cases {
		if got := formatElapsed(tc.d); got != tc.want {
			t.Errorf("formatElapsed(%v) = %q, want %q", tc.d, got, tc.want)
		}
	}
}
