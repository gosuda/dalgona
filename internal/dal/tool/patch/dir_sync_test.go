package patch

import (
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

var errInjectedDirSync = errors.New("injected directory sync failure")

func stubDirectorySync(t *testing.T, stub func(*dirPin) error) {
	t.Helper()
	previous := syncPinnedDir
	syncPinnedDir = stub
	t.Cleanup(func() { syncPinnedDir = previous })
}

func failDirectorySync(t *testing.T, calls *int) {
	t.Helper()
	stubDirectorySync(t, func(*dirPin) error {
		*calls++
		return errInjectedDirSync
	})
}

func mintedSequence(t *testing.T, store *snapshot.Store) uint64 {
	t.Helper()
	ref := store.Put("probe.bin", []byte("probe"), nil)
	_, tail, found := strings.Cut(ref, ".")
	sequence, err := strconv.ParseUint(tail, 36, 64)
	if !found || err != nil {
		t.Fatalf("reference %q has no sequence: %v", ref, err)
	}
	return sequence
}

func requireNothingMinted(t *testing.T, store *snapshot.Store, before uint64) {
	t.Helper()
	if minted := mintedSequence(t, store) - before - 1; minted != 0 {
		t.Fatalf("the failed call minted %d references, want none", minted)
	}
}

func requireSyncFailureText(t *testing.T, err error, paths string) {
	t.Helper()
	text := err.Error()
	for _, part := range []struct {
		name string
		ok   bool
	}{
		{"start with the C13 words", strings.HasPrefix(text, "patch: the files were written but a directory sync failed: ")},
		{"carry the cause", strings.Contains(text, errInjectedDirSync.Error())},
		{"end with the affected paths", strings.HasSuffix(text, ". Affected paths: "+paths+".")},
	} {
		if !part.ok {
			t.Fatalf("error does not %s: %q", part.name, text)
		}
	}
}

func requireInstalled(t *testing.T, cwd string, images map[string]image) {
	t.Helper()
	for name, item := range images {
		if actual := readFixture(t, cwd, name); actual != item.after {
			t.Fatalf("%s = %q, want the installed bytes %q", name, actual, item.after)
		}
	}
}

func requireOldReferenceRefused(t *testing.T, f *outcomeFixture, style Style) {
	t.Helper()
	writeFixture(t, f.cwd, "a.txt", "a-before\n")
	err := f.run(t, style, "[a.txt@{refA}]\nPUT 1:\n+a-second\n")
	requireWrappedOutcome(t, err, style, withRecovery(refused(tool.StatusConflict, tool.ReasonPathChanged, tool.ErrClassStale), readUnknown("a.txt")))
	if actual := readFixture(t, f.cwd, "a.txt"); actual != "a-before\n" {
		t.Fatalf("a refused call changed a.txt to %q", actual)
	}
}

func requireCompleteSyncFailure(t *testing.T, style Style) {
	t.Helper()
	images := map[string]image{
		"a.txt": {"a-before\n", "a-after\n"},
		"b.txt": {"b-before\n", "b-after\n"},
	}
	f := newOutcomeFixture(t)
	twoFiles(t, f)
	calls := 0
	failDirectorySync(t, &calls)
	before := mintedSequence(t, f.env.Snapshots)
	err := f.run(t, style, twoFileEdit)
	if err == nil {
		t.Fatal("the call succeeded although its directory sync failed")
	}
	want := withPaths(writeFailure(tool.CommitComplete, tool.ReasonSyncFailed), "a.txt", "after", "b.txt", "after")
	requireWrappedOutcome(t, err, style, want)
	requireStatesMatchDisk(t, f.cwd, outcomeOfError(t, err).Paths, images)
	requireSyncFailureText(t, err, "a.txt, b.txt")
	if calls != 2 {
		t.Fatalf("directory syncs attempted = %d, want one for each of the two pinned files", calls)
	}
	requireInstalled(t, f.cwd, images)
	requireNoResidue(t, f.cwd)
	requireNothingMinted(t, f.env.Snapshots, before)
	requireOldReferenceRefused(t, f, style)
}

func TestDirectorySyncFailureAfterRenamesIsCompleteWriteError(t *testing.T) {
	for _, style := range profileStyles {
		t.Run("IO-05/"+string(style), func(t *testing.T) { requireCompleteSyncFailure(t, style) })
	}
}

func TestDirectorySyncFailureStatesComeFromTheDisk(t *testing.T) {
	tests := []struct {
		name  string
		body  string
		keep  bool
		want  tool.Outcome
		files map[string]string
	}{
		{"IO-05/reverted during the sync", "b-before\n", false, withPaths(writeFailure(tool.CommitPartial, tool.ReasonSyncFailed), "a.txt", "after", "b.txt", "before"), nil},
		{"IO-05/replaced during the sync", "b-other\n", true, withPaths(commitUnknown, "a.txt", "after", "b.txt", "unknown"), map[string]string{"a.txt": "a-before\n", "b.txt": "b-before\n"}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			f := newOutcomeFixture(t)
			twoFiles(t, f)
			calls := 0
			stubDirectorySync(t, func(*dirPin) error {
				calls++
				if calls == 1 {
					writeFixture(t, f.cwd, "b.txt", test.body)
				}
				return errInjectedDirSync
			})
			before := mintedSequence(t, f.env.Snapshots)
			err := f.run(t, HashlineLight, twoFileEdit)
			if err == nil {
				t.Fatal("the call succeeded although its directory sync failed")
			}
			requireWrappedOutcome(t, err, HashlineLight, test.want)
			requireNothingMinted(t, f.env.Snapshots, before)
			if test.keep {
				for name, content := range test.files {
					requireBackupHolds(t, err.Error(), name, content)
				}
			} else {
				requireNoResidue(t, f.cwd)
			}
		})
	}
}

func requireTargetsAtSync(t *testing.T, cwd string, want map[string]string) {
	t.Helper()
	for name, body := range want {
		data, err := os.ReadFile(filepath.Join(cwd, name))
		if body == absent {
			if err == nil {
				t.Errorf("at sync time %s exists with %q, want it absent", name, data)
			}
			continue
		}
		if err != nil || string(data) != body {
			t.Errorf("at sync time %s = %q, %v; want %q", name, data, err, body)
		}
	}
}

func requireNoScratchAtSync(t *testing.T, cwd string) {
	t.Helper()
	var left []string
	for _, pattern := range []string{"*.dalgona-*", "*/*.dalgona-*"} {
		matches, err := filepath.Glob(filepath.Join(cwd, pattern))
		if err != nil {
			t.Error(err)
		}
		left = append(left, matches...)
	}
	if len(left) != 0 {
		t.Errorf("at sync time scratch files remain: %v", left)
	}
}

type syncOrderCase struct {
	name    string
	input   string
	targets map[string]string
	syncs   int
	dirs    []string
}

func requireSyncAfterLastOperation(t *testing.T, style Style, test syncOrderCase) {
	t.Helper()
	f := newOutcomeFixture(t)
	twoFiles(t, f)
	calls := 0
	synced := make(map[string]int)
	stubDirectorySync(t, func(pin *dirPin) error {
		calls++
		synced[pin.file.Name()]++
		requireTargetsAtSync(t, f.cwd, test.targets)
		requireNoScratchAtSync(t, f.cwd)
		return nil
	})
	if err := f.run(t, style, test.input); err != nil {
		t.Fatalf("the call failed: %v", err)
	}
	if calls != test.syncs {
		t.Fatalf("directory syncs = %d, want %d", calls, test.syncs)
	}
	if test.dirs != nil {
		want := make(map[string]int)
		for _, dir := range test.dirs {
			want[filepath.Join(f.cwd, dir)]++
		}
		for dir, count := range want {
			if synced[dir] != count {
				t.Errorf("directory %s synced %d times, want %d (all: %v)", dir, synced[dir], count, synced)
			}
		}
		for dir := range synced {
			if _, ok := want[dir]; !ok {
				t.Errorf("unexpected directory sync of %s (all: %v)", dir, synced)
			}
		}
	}
	requireNoResidue(t, f.cwd)
}

func TestDirectorySyncRunsAfterTheLastOperation(t *testing.T) {
	tests := []syncOrderCase{
		{"IO-05/two updates", twoFileEdit, map[string]string{"a.txt": "a-after\n", "b.txt": "b-after\n"}, 2, nil},
		{"IO-05/rename in place", "[a.txt@{refA}]\nMV z.txt\n", map[string]string{"a.txt": absent, "z.txt": "a-before\n", "b.txt": "b-before\n"}, 2, nil},
		{"IO-05/rename into a new directory", "[a.txt@{refA}]\nMV sub/z.txt\n", map[string]string{"a.txt": absent, "sub/z.txt": "a-before\n"}, 3, []string{"", "sub", ""}},
		{"IO-05/move into nested missing directories", "[a.txt@{refA}]\nMV out/generated/a.txt\n", map[string]string{"a.txt": absent, "out/generated/a.txt": "a-before\n"}, 4, []string{"out/generated", "", "out", ""}},
		{"IO-05/delete", "[a.txt@{refA}]\nREM\n", map[string]string{"a.txt": absent, "b.txt": "b-before\n"}, 1, nil},
	}
	for _, test := range tests {
		for _, style := range profileStyles {
			t.Run(test.name+"/"+string(style), func(t *testing.T) { requireSyncAfterLastOperation(t, style, test) })
		}
	}
}

func TestDirectorySyncAttemptsEveryPinAfterAFailure(t *testing.T) {
	t.Run("IO-05/rename source and destination", func(t *testing.T) {
		f := newOutcomeFixture(t)
		twoFiles(t, f)
		calls := 0
		failDirectorySync(t, &calls)
		err := f.run(t, HashlineLight, "[a.txt@{refA}]\nMV sub/z.txt\n")
		if err == nil {
			t.Fatal("the call succeeded although its directory sync failed")
		}
		want := withPaths(writeFailure(tool.CommitComplete, tool.ReasonSyncFailed), "a.txt", "after", "sub/z.txt", "after")
		requireWrappedOutcome(t, err, HashlineLight, want)
		if calls != 3 {
			t.Fatalf("directory syncs attempted = %d, want the source, the destination, and the parent of the created directory", calls)
		}
	})
}

func TestLegacyStylesDoNotSyncDirectories(t *testing.T) {
	t.Run("COMPAT-01/replace", func(t *testing.T) {
		env, cwd := editTestEnv(t)
		writeFixture(t, cwd, "a.txt", "a-before\n")
		writeFixture(t, cwd, "b.txt", "b-before\n")
		stubDirectorySync(t, func(*dirPin) error {
			t.Error("a legacy call synced a directory")
			return errInjectedDirSync
		})
		input := `{"changes":[{"path":"a.txt","old":"a-before","new":"a-after"},{"path":"b.txt","old":"b-before","new":"b-after"}]}`
		if _, err := runPatch(t, env, Replace, input); err != nil {
			t.Fatalf("the legacy call failed: %v", err)
		}
		requireInstalled(t, cwd, map[string]image{"a.txt": {"a-before\n", "a-after\n"}, "b.txt": {"b-before\n", "b-after\n"}})
		requireNoResidue(t, cwd)
	})
}
