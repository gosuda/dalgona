//go:build gui

package shell

import (
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func TestHeaderStateWords(t *testing.T) {
	approval := state.RequestView{Key: state.RequestKey{Request: "r1"}, Method: "item/commandExecution/requestApproval", Actionable: true}
	question := state.RequestView{Key: state.RequestKey{Request: "r2"}, Method: "item/tool/requestUserInput", Actionable: true}
	cases := []struct {
		name  string
		tv    state.ThreadView
		phase client.Phase
		want  string
	}{
		{"never prompted", state.ThreadView{Thread: rpc.Thread{Status: rpc.ThreadStatus{Type: "notLoaded"}}}, client.PhaseConnected, "Ready"},
		{"running", state.ThreadView{ActiveTurn: "t1", Turns: []state.TurnView{{ID: "t1", Status: "inProgress"}}}, client.PhaseConnected, "Working"},
		{"approval", state.ThreadView{ActiveTurn: "t1", Turns: []state.TurnView{{ID: "t1", Status: "inProgress"}}, Requests: []state.RequestView{approval}}, client.PhaseConnected, "Waiting · needs approval"},
		{"question", state.ThreadView{Requests: []state.RequestView{question}}, client.PhaseConnected, "Waiting · needs an answer"},
		{"done", state.ThreadView{Turns: []state.TurnView{{ID: "t1", Status: "completed"}}}, client.PhaseConnected, "Done"},
		{"failed", state.ThreadView{Turns: []state.TurnView{{ID: "t1", Status: "failed"}}}, client.PhaseConnected, "Failed"},
		{"interrupted", state.ThreadView{Turns: []state.TurnView{{ID: "t1", Status: "interrupted"}}}, client.PhaseConnected, "Stopped"},
		{"offline keeps nothing else", state.ThreadView{Turns: []state.TurnView{{ID: "t1", Status: "completed"}}}, client.PhaseOffline, "Offline"},
		{"unknown turn status", state.ThreadView{Turns: []state.TurnView{{ID: "t1", Status: "paused"}}}, client.PhaseConnected, "paused"},
	}
	for _, c := range cases {
		if got := threadTag(&c.tv, c.phase).Label(); got != c.want {
			t.Errorf("%s: %q, want %q", c.name, got, c.want)
		}
	}
}

func TestOfflineKeepsTheRecordedOutcome(t *testing.T) {
	tv := &state.ThreadView{Turns: []state.TurnView{{ID: "t1", Status: "failed"}}}
	if got := threadTag(tv, client.PhaseOffline).Name(); got != "Offline" {
		t.Fatalf("offline header = %q", got)
	}
	if got := threadTag(tv, client.PhaseConnected).Name(); got != "Failed" {
		t.Fatalf("after reconnect the outcome reads %q, want Failed", got)
	}
}

func TestItemStateWords(t *testing.T) {
	cases := map[string]string{"completed": "Done", "failed": "Failed", "declined": "Declined", "timedOut": "Timed out", "inProgress": "Working", "interrupted": "Stopped", "queuedRemotely": "queuedRemotely"}
	for status, want := range cases {
		if got := ItemTag(status).Name(); got != want {
			t.Errorf("ItemTag(%q) = %q, want %q", status, got, want)
		}
	}
}

func TestDurationForms(t *testing.T) {
	cases := map[time.Duration]string{
		420 * time.Millisecond:                "420 ms",
		59*time.Second + 999*time.Millisecond: "59s",
		61 * time.Second:                      "1m 01s",
		59*time.Minute + 5*time.Second:        "59m 05s",
		3*time.Hour + 4*time.Minute:           "3h 04m",
	}
	for d, want := range cases {
		if got := formatDuration(d); got != want {
			t.Errorf("formatDuration(%v) = %q, want %q", d, got, want)
		}
	}
	if got, want := formatRetry(75*time.Second), "rate limited · retrying in 1m 15s · esc stops"; got != want {
		t.Errorf("formatRetry = %q, want %q", got, want)
	}
}

func TestWorkingTagCountsFromTheServerStart(t *testing.T) {
	start := time.Unix(1_800_000_000, 0)
	tv := &state.ThreadView{ActiveTurn: "t1", Turns: []state.TurnView{{ID: "t1", Status: "inProgress", StartedAt: start.Unix()}}}
	tag := threadTag(tv, client.PhaseConnected)
	if got := liveClock(start.Add(65*time.Second+300*time.Millisecond), tag); got != "1m 05s" {
		t.Errorf("elapsed = %q, want 1m 05s", got)
	}
	fresh := &state.ThreadView{ActiveTurn: "t2", Turns: []state.TurnView{{ID: "t1", Status: "completed", StartedAt: start.Unix()}, {ID: "t2", Status: "inProgress"}}}
	if got := liveClock(start.Add(time.Hour), threadTag(fresh, client.PhaseConnected)); got != "" {
		t.Errorf("a new turn without a start shows %q, want empty", got)
	}
	wait := &state.ThreadView{ActiveTurn: "t1", Turns: []state.TurnView{{ID: "t1", Status: "inProgress", StartedAt: start.Unix(), RetryWait: 90 * time.Second, RetryWaitAt: start}}}
	if got := liveClock(start.Add(30*time.Second), threadTag(wait, client.PhaseConnected)); got != "rate limited · retrying in 1m 00s · esc stops" {
		t.Errorf("rate-limit text = %q", got)
	}
}

func TestBreathCycleStaysAboveItsFloor(t *testing.T) {
	start := time.Unix(0, 0)
	low := float32(1)
	for step := range 180 {
		a := breath(start.Add(time.Duration(step) * 10 * time.Millisecond))
		low = min(low, a)
		if a < 0.4-1e-3 || a > 1+1e-3 {
			t.Fatalf("opacity %v at step %d leaves [0.4, 1]", a, step)
		}
	}
	if low > 0.41 {
		t.Errorf("the cycle never reaches its 0.4 floor; lowest %v", low)
	}
	if a := breath(start.Add(1800 * time.Millisecond)); a < 0.999 {
		t.Errorf("a full 1.8 s cycle ends at %v, want 1", a)
	}
}
