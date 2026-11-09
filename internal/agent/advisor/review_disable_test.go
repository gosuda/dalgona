package advisor

import "testing"

func TestComplete_stopsDeliveringNotesWhenDisabledBetweenNotes(t *testing.T) {
	h := newHarness(t, nil)
	h.model.push(roleAdvisor,
		calls(advise("n1", "first nit", "nit"), advise("n2", "second nit", "nit")),
		text("done"))
	thread, rt := h.start()
	a := rt.advisors[0]

	read, err := rt.primary.ReadAfter(0)
	if err != nil {
		t.Fatal(err)
	}
	child, err := a.ensureChild(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if err := a.submit(t.Context(), reviewInput{text: reviewPrompt(read.Head, "review the change"), head: read.Head}); err != nil {
		t.Fatal(err)
	}
	h.waitIdle(child)

	a.mu.Lock()
	current := a.current
	a.mu.Unlock()

	testHook = func() { a.setEnabled(false, read.Head) }
	defer func() { testHook = nil }()

	if err := a.complete(t.Context(), current); err != nil {
		t.Fatalf("complete = %v, want a clean stop that records no failure", err)
	}
	notes := notesIn(journalEvents(t, h.store, thread.ID()))
	if len(notes) != 1 || notes[0].Note != "first nit" {
		t.Fatalf("notes = %+v, want only the note delivered before the disable", notes)
	}
}

func TestAdmitBlocker_discardsAfterAdmissionWhenDisabled(t *testing.T) {
	h := newHarness(t, nil)
	_, rt := h.start()
	a := rt.advisors[0]
	if _, err := a.ensureChild(t.Context()); err != nil {
		t.Fatal(err)
	}
	a.guard.BeginReview()

	testHook = func() { a.setEnabled(false, 0) }
	defer func() { testHook = nil }()

	verdict, err := a.admitBlocker(t.Context(), advice{note: "drop the column", severity: SeverityBlocker})
	if err != nil {
		t.Fatal(err)
	}
	if verdict != verdictDiscarded {
		t.Fatalf("verdict = %q, want %q", verdict, verdictDiscarded)
	}
	if notes := notesIn(journalEvents(t, h.store, rt.primary.ID())); len(notes) != 0 {
		t.Fatalf("notes = %+v, want none delivered after the disable", notes)
	}
}
