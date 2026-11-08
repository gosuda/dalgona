package agent

import "testing"

func infoModel(t *testing.T, host *Host, threadID string) string {
	t.Helper()
	for _, info := range host.AllThreads() {
		if info.ID == threadID {
			return info.Model
		}
	}
	t.Fatalf("thread %s is not listed", threadID)
	return ""
}

func expectModel(t *testing.T, h *harness, thread *Thread, want string) {
	t.Helper()
	current, err := thread.Settings()
	if err != nil {
		t.Fatal(err)
	}
	if current.Model != want {
		t.Fatalf("effective model = %q, want %q", current.Model, want)
	}
	if got := infoModel(t, h.host, thread.ID()); got != want {
		t.Fatalf("ThreadInfo.Model = %q, want %q", got, want)
	}
}

func TestThreadInfoModelFollowsLeafBackToOverrideBranch(t *testing.T) {
	h := newHarness(t)
	thread := h.start()
	h.submit(thread, "one")
	h.waitIdle(thread)
	if err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "two", Model: "openai/gpt-y"}); err != nil {
		t.Fatal(err)
	}
	h.waitIdle(thread)
	override := turnIDs(h.events(thread.ID()))[1]
	expectModel(t, h, thread, "openai/gpt-y")
	if err := thread.Revert(override); err != nil {
		t.Fatal(err)
	}
	h.submit(thread, "three")
	h.waitIdle(thread)
	expectModel(t, h, thread, "openai/gpt-test")
	if err := thread.SetLeaf(override); err != nil {
		t.Fatal(err)
	}
	expectModel(t, h, thread, "openai/gpt-y")
	if err := h.host.Close(); err != nil {
		t.Fatal(err)
	}
	h.host = h.newHost()
	if got := infoModel(t, h.host, thread.ID()); got != "openai/gpt-y" {
		t.Fatalf("scanned ThreadInfo.Model = %q, want openai/gpt-y", got)
	}
}
