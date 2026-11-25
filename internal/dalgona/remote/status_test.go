package remote

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

func TestStatusListsRunnerBindings(t *testing.T) {
	ctx := t.Context()
	store := openJournal(t)
	b, box := newFakeBinder(t, store)
	startThread(t, store, "thread-a", t.TempDir())
	box.use(ctx, t, "thread-a")
	startFork(t, store, "thread-b", "thread-a")
	box.use(ctx, t, "thread-b")
	startThread(t, store, "thread-c", t.TempDir())

	status, err := b.Status(ctx, "")
	must(t, err)
	requireRunners(t, status.Runners)
	requireSharedBinding(t, status.Bindings)

	box.archive(ctx, t, "thread-a", 0)
	only, err := b.Status(ctx, "thread-a")
	must(t, err)
	requireSingleBinding(t, only.Bindings, "archived", 1)
	unbound, err := b.Status(ctx, "thread-c")
	must(t, err)
	requireSingleBinding(t, unbound.Bindings, "unbound", 0)
	preview, err := b.GCPreview()
	must(t, err)
	if preview.PreviewRevision != only.LedgerRevision {
		t.Fatalf("gc preview revision %q differs from status ledger revision %q", preview.PreviewRevision, only.LedgerRevision)
	}
}

func requireRunners(t *testing.T, runners []RunnerStatus) {
	t.Helper()
	names := []string{}
	for _, r := range runners {
		names = append(names, r.Name)
	}
	if !slices.Equal(names, []string{"local", "box"}) || !runners[1].Available {
		t.Fatalf("runners %+v, want available local and box", runners)
	}
}

func requireSharedBinding(t *testing.T, bindings []BindingStatus) {
	t.Helper()
	if len(bindings) != 2 {
		t.Fatalf("bindings %+v, want thread-a and thread-b", bindings)
	}
	first := bindings[0]
	want := BindingStatus{
		ThreadID: "thread-a", RunnerID: "box", BoxID: "dalgona-thread-a", SharedThreadIDs: []string{"thread-b"}, State: "bound",
		BindingRevision: bindingDigest("box", event.RunnerBound{Runner: "box", Service: "fake", BoxID: "dalgona-thread-a", Baseline: "baseline-1"}),
	}
	if !reflect.DeepEqual(first, want) {
		t.Fatalf("binding %+v, want %+v", first, want)
	}
}

func requireSingleBinding(t *testing.T, bindings []BindingStatus, state string, shared int) {
	t.Helper()
	if len(bindings) != 1 || bindings[0].State != state || len(bindings[0].SharedThreadIDs) != shared {
		t.Fatalf("bindings %+v, want one %s binding shared with %d threads", bindings, state, shared)
	}
}

func bindingDigest(runnerName string, bound event.RunnerBound) string {
	var fields bytes.Buffer
	for _, field := range []string{runnerName, bound.Runner, bound.Service, bound.BoxID, bound.Baseline} {
		fields.WriteString(field)
		fields.WriteByte(0)
	}
	sum := sha256.Sum256(fields.Bytes())
	return hex.EncodeToString(sum[:])
}

func TestGCConfirmationRefusesChangedLedger(t *testing.T) {
	ctx := t.Context()
	store := openJournal(t)
	b, box := newFakeBinder(t, store)
	startThread(t, store, "thread-a", t.TempDir())
	box.use(ctx, t, "thread-a")
	must(t, appendEvents(ctx, store, "thread-a", event.ThreadArchived{Archived: true}))
	preview, err := b.GCPreview()
	must(t, err)
	if len(preview.Candidates) != 1 {
		t.Fatalf("candidates %+v, want the archived thread's box", preview.Candidates)
	}
	startThread(t, store, "thread-b", t.TempDir())
	box.use(ctx, t, "thread-b")
	must(t, appendEvents(ctx, store, "thread-b", event.ThreadArchived{Archived: true}))
	_, err = b.GCConfirmed(ctx, preview.PreviewRevision)
	if _, stale := errors.AsType[*StalePreviewError](err); !stale || box.svc.DestroyCount() != 0 {
		t.Fatalf("confirmation with an old preview: %v, destroyed %d; want stale refusal before destruction", err, box.svc.DestroyCount())
	}
	current, err := b.GCPreview()
	must(t, err)
	done, err := b.GCConfirmed(ctx, current.PreviewRevision)
	must(t, err)
	if len(done.Destroyed) != 2 || box.svc.DestroyCount() != 2 {
		t.Fatalf("destroyed %+v (%d), want both boxes", done.Destroyed, box.svc.DestroyCount())
	}
}

func TestPullProgressPhasesArriveInOrder(t *testing.T) {
	box := gitWorkspace(t)
	pinLF(t, box)
	must(t, os.WriteFile(filepath.Join(box, "hello.txt"), []byte("two from box\n"), 0o644))
	diffCmd := exec.Command("git", "diff", "--no-ext-diff", "--no-color", "--binary")
	diffCmd.Dir = box
	diff, err := diffCmd.Output()
	must(t, err)
	local := gitWorkspace(t)
	pinLF(t, local)
	b := NewBinder(Options{})
	var phases []client.ProgressPhase
	var result PullResult
	err = b.merge(t.Context(), local, diff, &result, func(phase client.ProgressPhase, completed, total uint64) {
		phases = append(phases, phase)
		if total != 1 {
			t.Errorf("%s total %d, want one changed file", phase, total)
		}
	})
	must(t, err)
	if want := []client.ProgressPhase{client.ProgressMerging, client.ProgressPublishing, client.ProgressFinished}; !slices.Equal(phases, want) {
		t.Fatalf("phases %v, want %v", phases, want)
	}
	if !slices.Equal(result.ChangedPaths, []string{"hello.txt"}) {
		t.Fatalf("changed paths %v, want hello.txt", result.ChangedPaths)
	}
	requireFiles(t, local, map[string]string{"hello.txt": "two from box\n"})
}
