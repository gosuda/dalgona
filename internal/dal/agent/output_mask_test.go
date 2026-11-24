package agent

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/exec"
)

func TestLiveExecDefersDetachedBuiltinValue(t *testing.T) {
	const progress = "ordinary progress\n"
	value := "AKIA" + strings.Repeat("Q", 16)
	first := progress + "x" + value + "\n"
	second := "match " + value + "\n"
	w, tee := liveExecFixture(t, first+second)
	readVerifiedPage(t, tee, 0, int64(len(first)))
	assertLiveDelta(t, w.t, progress)
	readVerifiedPage(t, tee, int64(len(first)), int64(len(second)))
	assertLiveDelta(t, w.t, "")
	const suffix = "x{{secret:builtin0:0}}\nmatch {{secret:builtin0:0}}\n"
	final, err := completeLiveExec(t, w, tee, first+second, nil)
	if err != nil || final.Output != progress+suffix {
		t.Fatalf("wrong verified final output, length %d, error %v", len(final.Output), err)
	}
	assertLiveDelta(t, w.t, suffix)
}

func TestLiveExecMasksInlineOutputWithDiagnosticBeforeRelease(t *testing.T) {
	const raw = "credential-part"
	const diagnostic = "continuation"
	t.Setenv("DAL_LIVE_DIAGNOSTIC_PASSWORD", raw+"\n"+diagnostic)
	w, tee := liveExecFixture(t, raw)
	readVerifiedPage(t, tee, 0, int64(len(raw)+1))
	assertLiveDelta(t, w.t, "")
	final, err := completeLiveExec(t, w, tee, raw, errors.New(diagnostic))
	const placeholder = "{{secret:DAL_LIVE_DIAGNOSTIC_PASSWORD}}"
	if err != nil || final.OK || final.Output != placeholder {
		t.Fatalf("unmasked diagnostic, final length %d, error %v", len(final.Output), err)
	}
	assertLiveDelta(t, w.t, placeholder)
}

func TestLiveExecFencesChangedCredentialView(t *testing.T) {
	const first = "already-observed-first-line\n"
	const second = "newly-protected-second-line\n"
	for _, row := range []struct {
		name, secret, completion, finalDelta string
	}{
		{"prefix no longer agrees", strings.TrimSuffix(first, "\n"), "{{secret:DAL_LIVE_ROTATE_PASSWORD}}\n" + second, ""},
		{"pending line became a credential", strings.TrimSuffix(second, "\n"), first + "{{secret:DAL_LIVE_ROTATE_PASSWORD}}\n", "{{secret:DAL_LIVE_ROTATE_PASSWORD}}\n"},
	} {
		t.Run(row.name, func(t *testing.T) {
			w, tee := liveExecFixture(t, first+second)
			readVerifiedPage(t, tee, 0, int64(len(first)))
			assertLiveDelta(t, w.t, first)
			t.Setenv("DAL_LIVE_ROTATE_PASSWORD", row.secret)
			readVerifiedPage(t, tee, int64(len(first)), int64(len(second)))
			assertLiveDelta(t, w.t, "")
			final, err := completeLiveExec(t, w, tee, first+second, nil)
			if err != nil || final.Output != row.completion {
				t.Fatalf("wrong current-epoch completion, length %d, error %v", len(final.Output), err)
			}
			assertLiveDelta(t, w.t, row.finalDelta)
		})
	}
}

func TestLiveExecNeverReleasesUnpersistedPlaceholder(t *testing.T) {
	value := "AKIA" + strings.Repeat("Q", 16) + "\n"
	w, home := spillTestWorker(t)
	tee := prepareLiveExec(w, value)
	if err := os.Mkdir(filepath.Join(home, "secrets.json"), 0o700); err != nil {
		t.Fatal(err)
	}
	readVerifiedPage(t, tee, 0, int64(len(value)))
	if _, err := completeLiveExec(t, w, tee, value, nil); err == nil || !strings.Contains(err.Error(), "persist pattern secrets") {
		t.Fatalf("completion masking error = %v, want persistence failure", err)
	}
	assertLiveDelta(t, w.t, "")
}

func TestLiveExecDoesNotRelayAfterUnseenBytes(t *testing.T) {
	const raw = "known\nunseen\notherwise ordinary\n"
	w, tee := liveExecFixture(t, raw)
	readVerifiedPage(t, tee, 0, int64(len("known\n")))
	assertLiveDelta(t, w.t, "known\n")
	readVerifiedPage(t, tee, int64(len("known\nunseen\n")), int64(len("otherwise ordinary\n")))
	assertLiveDelta(t, w.t, "")
}

func liveExecFixture(t *testing.T, raw string) (*worker, *outputTee) {
	t.Helper()
	w, _ := spillTestWorker(t)
	return w, prepareLiveExec(w, raw)
}

func prepareLiveExec(w *worker, raw string) *outputTee {
	w.t.runner = pemExitRunner{output: raw}
	w.t.output.ch = make(chan Update, outputBuffer)
	return w.t.teeOutput(w.t.runner, "exec", "exec")
}

func completeLiveExec(t *testing.T, w *worker, tee *outputTee, raw string, runErr error) (event.ToolCompleted, error) {
	t.Helper()
	p := &prepared{call: event.ToolCall{ID: "exec", Name: "exec"}, class: tool.ClassExec, tool: exec.New(), execOutput: tee}
	result := tool.Result{OK: runErr == nil, Output: []byte(raw), Path: spillFixturePath}
	return w.completion(t.Context(), p, result, runErr)
}

func assertLiveDelta(t *testing.T, thread *Thread, want string) {
	t.Helper()
	if text := strings.Join(drained(thread), ""); text != want {
		t.Fatalf("live relay yielded %d bytes, want %d verified bytes", len(text), len(want))
	}
}
