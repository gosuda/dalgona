package cursor

import (
	"testing"
)

func TestAbortEvictsOldestAndKeepsNewest(t *testing.T) {
	t.Parallel()
	session := &session{}
	var cancels []error
	stop := func(cause error) { cancels = append(cancels, cause) }
	for id := uint32(1); id <= maxQueuedFrames; id++ {
		session.abort(id, 1<<20, stop)
	}
	const newest = uint32(maxQueuedFrames + 1)
	session.abort(newest, 1<<20, stop)
	if len(session.aborted) != maxQueuedFrames {
		t.Fatalf("len(aborted) = %d, want %d", len(session.aborted), maxQueuedFrames)
	}
	if session.aborted[0] != 2 || session.aborted[maxQueuedFrames-1] != newest {
		t.Fatalf("aborted = [%d ... %d], want oldest 2 evicted 1 and newest %d kept", session.aborted[0], session.aborted[maxQueuedFrames-1], newest)
	}
	if !session.takeAborted(newest) {
		t.Fatal("takeAborted(newest) = false, want true")
	}
	if session.takeAborted(1) {
		t.Fatal("takeAborted(1) = true, want evicted oldest to be gone")
	}
	if len(cancels) != 0 {
		t.Fatalf("cancels = %v, want none for queued aborts", cancels)
	}
}

func TestAbortCancelsRunningExec(t *testing.T) {
	t.Parallel()
	session := &session{}
	const execID = uint32(7)
	var cancels []error
	session.abort(execID, execID, func(cause error) { cancels = append(cancels, cause) })
	if len(cancels) != 1 || session.aborted != nil {
		t.Fatalf("cancels = %v, aborted = %v; want one cancel and no queued id", cancels, session.aborted)
	}
}
