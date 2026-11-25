package transcript

import "testing"

func TestStateWordMapsStatuses(t *testing.T) {
	cases := []struct {
		name   string
		status string
		want   string
	}{
		{name: "active", status: "active", want: "Working"},
		{name: "progress", status: "inProgress", want: "Working"},
		{name: "awaiting", status: "awaitingApproval", want: "Waiting"},
		{name: "done", status: "completed", want: "Done"},
		{name: "failed", status: "failed", want: "Failed"},
		{name: "timed out", status: "timedOut", want: "Timed out"},
		{name: "unknown", status: "bogus", want: "bogus"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := stateWord(c.status); got != c.want {
				t.Fatalf("stateWord(%q) reads %q, want %q", c.status, got, c.want)
			}
		})
	}
}

func TestPlanWordMapsStatuses(t *testing.T) {
	cases := []struct {
		name   string
		status string
		want   string
	}{
		{name: "done", status: "completed", want: "Done"},
		{name: "working", status: "inProgress", want: "Working"},
		{name: "pending", status: "pending", want: "Pending"},
		{name: "unknown", status: "bogus", want: "bogus"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := planWord(c.status); got != c.want {
				t.Fatalf("planWord(%q) reads %q, want %q", c.status, got, c.want)
			}
		})
	}
}

func TestLineCountCountsTrimmedLines(t *testing.T) {
	cases := []struct {
		name string
		text string
		want int
	}{
		{name: "empty", text: "", want: 0},
		{name: "newline", text: "\n", want: 0},
		{name: "one", text: "a", want: 1},
		{name: "one nl", text: "a\n", want: 1},
		{name: "two", text: "a\nb", want: 2},
		{name: "two nl", text: "a\nb\n", want: 2},
		{name: "blank tail", text: "a\n\n", want: 1},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := lineCount(c.text); got != c.want {
				t.Fatalf("lineCount(%q) reads %d, want %d", c.text, got, c.want)
			}
		})
	}
}
