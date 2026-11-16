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

func TestFormatResumeDate(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 15, 12, 0, 0, 0, time.UTC)
	cases := []struct {
		name string
		t    time.Time
		now  time.Time
		want string
	}{
		{"current year", time.Date(2026, 5, 3, 9, 0, 0, 0, time.UTC), now, "05-03"},
		{"earlier year", time.Date(2025, 11, 20, 9, 0, 0, 0, time.UTC), now, "2025-11"},
		{"future year", time.Date(2027, 1, 2, 9, 0, 0, 0, time.UTC), now, "2027-01"},
		{"zero time", time.Time{}, now, ""},
		{"year zero", time.Date(0, 3, 1, 9, 0, 0, 0, time.UTC), now, ""},
		{"year below one", time.Date(-44, 3, 15, 9, 0, 0, 0, time.UTC), now, ""},
		{"year one", time.Date(1, 3, 1, 9, 0, 0, 0, time.UTC), now, "0001-03"},
		{"year 9999", time.Date(9999, 12, 31, 9, 0, 0, 0, time.UTC), now, "9999-12"},
		{"year 10000", time.Date(10000, 1, 2, 9, 0, 0, 0, time.UTC), now, ""},
		{"new year in now zone", time.Date(2025, 12, 31, 23, 30, 0, 0, time.UTC), time.Date(2026, 11, 15, 12, 0, 0, 0, time.FixedZone("east", 13*3600)), "01-01"},
	}
	for _, tc := range cases {
		if got := formatResumeDate(tc.t, tc.now); got != tc.want {
			t.Errorf("formatResumeDate(%s) %s = %q, want %q", tc.t.Format(time.RFC3339), tc.name, got, tc.want)
		}
	}
}
