package search

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func requireIndexHost(t testing.TB) {
	t.Helper()
	requireChangeTimes(t)
	if runner.UnavailableCause() != "" {
		t.Skip("the index runs only where the sandbox enforces its writable roots")
	}
}

func newIndexLocal(t *testing.T) *runner.Local {
	t.Helper()
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	return local
}

func indexedEnv(t *testing.T, files map[string]string) (tool.Env, string) {
	t.Helper()
	env, root := buildEnv(t, files)
	publishIndexFor(t, env.Cwd, root)
	return env, root
}

func buildEnv(t *testing.T, files map[string]string) (tool.Env, string) {
	t.Helper()
	requireIndexHost(t)
	base := t.TempDir()
	if err := os.MkdirAll(filepath.Join(base, "tmp"), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("TMPDIR", filepath.Join(base, "tmp"))
	ws := filepath.Join(base, "ws")
	writeBuildFiles(t, ws, files)
	home := filepath.Join(base, "home")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatal(err)
	}
	root, err := indexRoot(home, ws)
	if err != nil {
		t.Fatal(err)
	}
	env := tool.Env{
		Runner: newIndexLocal(t), Cwd: ws, Home: home, Snapshots: snapshot.New(),
		Sandbox: runner.Sandbox{Mode: runner.SandboxWorkspaceWrite},
	}
	return env, root
}

func publishIndexFor(t *testing.T, ws, root string) string {
	t.Helper()
	start := time.Now()
	gen, dir, err := createGeneration(root, start)
	if err != nil {
		t.Fatal(err)
	}
	if err = buildIndex(ws, dir, start, 1<<30); err != nil {
		t.Fatal(err)
	}
	if err = publishGeneration(root, gen); err != nil {
		t.Fatal(err)
	}
	return gen
}

var (
	outputRef = regexp.MustCompile(`@r[0-9a-f]+\.[0-9a-z]+`)
	metaRef   = regexp.MustCompile(`"ref":"r[0-9a-f]+\.[0-9a-z]+"`)
)

func normalizeRefs(data []byte) string {
	text := outputRef.ReplaceAllString(string(data), "@REF")
	return metaRef.ReplaceAllString(text, `"ref":"REF"`)
}

type recordingRunner struct {
	runner.Runner
	ids  []string
	argv [][]string
}

func (r *recordingRunner) Start(ctx context.Context, spec runner.ProcessSpec) error {
	r.ids = append(r.ids, spec.ID)
	r.argv = append(r.argv, slices.Clone(spec.Argv))
	return r.Runner.Start(ctx, spec)
}

func TestIndexRootHashesCanonicalWorkspace(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("creating a symbolic link needs a privilege the test does not have")
	}
	base := t.TempDir()
	real := filepath.Join(base, "real")
	if err := os.Mkdir(real, 0o700); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(base, "link")
	if err := os.Symlink(real, link); err != nil {
		t.Fatal(err)
	}
	home := filepath.Join(base, "home")
	got, err := indexRoot(home, link)
	if err != nil {
		t.Fatal(err)
	}
	resolvedReal, err := filepath.EvalSymlinks(real)
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256([]byte(resolvedReal))
	want := filepath.Join(home, "search", hex.EncodeToString(sum[:8]))
	if got != want {
		t.Fatalf("indexRoot = %q, want %q", got, want)
	}
}

func TestForgetGenerationRemovesOnlyTheFailedOne(t *testing.T) {
	root := t.TempDir()
	writeBuildFiles(t, root, map[string]string{currentFile: "gen-b\n"})
	if err := forgetGeneration(root, "gen-a"); err != nil {
		t.Fatal(err)
	}
	requireCurrent(t, root, "gen-b")
	if err := forgetGeneration(root, "gen-b"); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(root, currentFile)); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("CURRENT still exists after its generation failed: %v", err)
	}
}

func TestCurrentNameOutsideRootIsIgnored(t *testing.T) {
	base := t.TempDir()
	root := filepath.Join(base, "root")
	if err := os.MkdirAll(filepath.Join(root, "x"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(filepath.Join(base, "escape"), 0o700); err != nil {
		t.Fatal(err)
	}
	writeBuildFiles(t, root, map[string]string{currentFile: "../escape\n"})
	if got := currentGeneration(root); got != "" {
		t.Fatalf("currentGeneration = %q, want no generation", got)
	}
}

func TestRemoteRunnerGetsNoIndexArguments(t *testing.T) {
	env, root := indexedEnv(t, map[string]string{"a.txt": "needle\n"})
	gen := currentGeneration(root)
	if gen == "" {
		t.Fatal("the fixture published no generation")
	}
	process := &fakeProcessRunner{Runner: env.Runner}
	env.Runner = process
	if _, err := New().Run(t.Context(), tool.Call{Args: jsontext.Value(`{"mode":"grep","pattern":"needle"}`)}, env); err != nil {
		t.Fatal(err)
	}
	if len(process.argv) != 1 || len(process.argv[0]) != 3 {
		t.Fatalf("remote runner got argv %q, want three elements", process.argv)
	}
}

func TestLocalGrepPassesGeneration(t *testing.T) {
	env, root := indexedEnv(t, map[string]string{"a.txt": "needle\n"})
	gen := currentGeneration(root)
	raw := jsontext.Value(`{"mode":"grep","pattern":"needle"}`)
	ref := indexFor(env)
	if ref.root != root || ref.gen != gen {
		t.Fatalf("indexFor = %+v, want root %q and generation %q", ref, root, gen)
	}
	before := time.Now()
	argv := searchArgs(raw, ref, time.Now())
	after := time.Now()
	requireGrepArgv(t, argv, ref, before, after)
	if find := searchArgs(jsontext.Value(`{"mode":"find","pattern":"*"}`), indexRef{}, before); len(find) != 3 {
		t.Fatalf("find argv = %q, want three elements", find)
	}
}

func requireGrepArgv(t *testing.T, argv []string, ref indexRef, before, after time.Time) {
	t.Helper()
	if len(argv) != 7 || argv[3] != ref.root || argv[4] != ref.gen || argv[6] != strconv.FormatInt(racyWindow.Nanoseconds(), 10) {
		t.Fatalf("grep argv = %q", argv)
	}
	clock, err := strconv.ParseInt(argv[5], 10, 64)
	if err != nil {
		t.Fatal(err)
	}
	if clock < before.UnixNano() || clock > after.UnixNano() {
		t.Fatalf("clock %d is outside [%d, %d]", clock, before.UnixNano(), after.UnixNano())
	}
}

func TestNoIndexWithoutSandbox(t *testing.T) {
	env, _ := indexedEnv(t, map[string]string{"a.txt": "needle\n"})
	env.Sandbox = runner.Sandbox{Mode: runner.SandboxOff}
	ref := indexFor(env)
	if ref.root != "" || ref.gen != "" {
		t.Fatalf("indexFor with the sandbox off = %+v", ref)
	}
	if argv := searchArgs(jsontext.Value(`{"mode":"grep","pattern":"needle"}`), ref, time.Now()); len(argv) != 3 {
		t.Fatalf("argv = %q, want three elements", argv)
	}
}

func TestNoIndexInsideWorkspace(t *testing.T) {
	requireIndexHost(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"a.txt": "needle\n"})
	home := filepath.Join(ws, ".dal")
	root, err := indexRoot(home, ws)
	if err != nil {
		t.Fatal(err)
	}
	publishIndexFor(t, ws, root)
	if currentGeneration(root) == "" {
		t.Fatal("the fixture published no generation")
	}
	env := tool.Env{Runner: newIndexLocal(t), Cwd: ws, Home: home, Snapshots: snapshot.New(), Sandbox: runner.Sandbox{Mode: runner.SandboxWorkspaceWrite}}
	if ref := indexFor(env); ref.gen != "" {
		t.Fatalf("indexFor inside the workspace = %+v", ref)
	}
}

func TestNoIndexWithEmptyHome(t *testing.T) {
	env, _ := indexedEnv(t, map[string]string{"a.txt": "needle\n"})
	env.Home = ""
	if ref := indexFor(env); ref.root != "" || ref.gen != "" {
		t.Fatalf("indexFor with an empty home = %+v", ref)
	}
}

func TestSandboxedChildCannotWriteIndexRoot(t *testing.T) {
	requireIndexHost(t)
	base := t.TempDir()
	tmp := filepath.Join(base, "tmp")
	if err := os.Mkdir(tmp, 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("TMPDIR", tmp)
	root := filepath.Join(base, "home", searchDir, "probe")
	if err := os.MkdirAll(root, 0o700); err != nil {
		t.Fatal(err)
	}
	local := newIndexLocal(t)
	if err := sandboxTouch(t, local, filepath.Join(tmp, "inside")); err != nil {
		t.Fatalf("the sandboxed child cannot write its temp directory, so the probe proves nothing: %v", err)
	}
	err := sandboxTouch(t, local, filepath.Join(root, currentFile))
	if err == nil || !strings.Contains(err.Error(), "subprocess exited") {
		t.Fatalf("sandboxed child wrote the index root: %v", err)
	}
}

func sandboxTouch(t *testing.T, local *runner.Local, path string) error {
	t.Helper()
	spec := runner.ProcessSpec{
		ID:      "sandbox-touch-" + filepath.Base(path),
		Argv:    []string{"sh", "-c", "touch '" + path + "'"},
		Dir:     t.TempDir(),
		Sandbox: runner.Sandbox{Mode: runner.SandboxWorkspaceWrite},
	}
	if err := local.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}
	_, err := collect(t.Context(), local, spec.ID)
	return err
}

func requireRetryMatchesScan(t *testing.T, damage func(trigram string)) {
	t.Helper()
	env, root := indexedEnv(t, map[string]string{"a.txt": "needle here\n", "b.txt": "other\n"})
	gen := currentGeneration(root)
	damage(filepath.Join(root, gen, trigramFile))
	raw := jsontext.Value(`{"mode":"grep","pattern":"needle","limit":100}`)
	parsed, err := parseArgs(raw)
	if err != nil {
		t.Fatal(err)
	}
	want, err := execute(env.Cwd, parsed, "", freshness{})
	if err != nil {
		t.Fatal(err)
	}
	got, err := New().Run(t.Context(), tool.Call{ID: "call-1", Args: raw}, env)
	if err != nil {
		t.Fatal(err)
	}
	wantResult := makeResult(want, snapshot.New())
	if normalizeRefs(got.Output) != normalizeRefs(wantResult.Output) {
		t.Fatalf("retry output %q, want %q", got.Output, wantResult.Output)
	}
	if normalizeRefs(got.Meta) != normalizeRefs(wantResult.Meta) {
		t.Fatalf("retry meta %s, want %s", got.Meta, wantResult.Meta)
	}
	if currentGeneration(root) != "" {
		t.Fatal("CURRENT still names the failed generation")
	}
}

func TestRunRetriesWithoutIndexOnStatusOne(t *testing.T) {
	requireRetryMatchesScan(t, func(trigram string) {
		info, err := os.Stat(trigram)
		if err != nil {
			t.Fatal(err)
		}
		if err = os.WriteFile(trigram, make([]byte, info.Size()), 0o600); err != nil {
			t.Fatal(err)
		}
	})
}

func TestTruncatedIndexRetriesToTodaysOutput(t *testing.T) {
	requireRetryMatchesScan(t, func(trigram string) {
		info, err := os.Stat(trigram)
		if err != nil {
			t.Fatal(err)
		}
		if err = os.Truncate(trigram, info.Size()-1); err != nil {
			t.Fatal(err)
		}
	})
}

func TestRetryUsesFreshProcessID(t *testing.T) {
	env, _ := indexedEnv(t, map[string]string{"a.txt": "needle\n"})
	recording := &recordingRunner{Runner: env.Runner}
	env.Runner = recording
	raw := jsontext.Value(`{"mode":"grep","pattern":"needle"}`)
	var err error
	if _, err = runSearch(t.Context(), env, "call-1", raw, indexRef{}); err != nil {
		t.Fatal(err)
	}
	if _, err = retryWithoutIndex(t.Context(), env, indexRef{root: "x", gen: "gen"}, raw, errors.New("cause")); err != nil {
		t.Fatal(err)
	}
	if len(recording.ids) != 2 || recording.ids[0] != "call-1" || recording.ids[1] == "call-1" {
		t.Fatalf("process ids %q, want call-1 and a fresh id", recording.ids)
	}
}

func TestOutcomeOfIndexFailureIsStatusOne(t *testing.T) {
	err := exitError{code: indexFailedStatus, output: []byte("search: index unusable: trigram size")}
	if !isIndexFailure(errors.Join(errors.New("wrapped"), err)) {
		t.Fatal("an index failure inside a joined error was not recognised")
	}
	if isIndexFailure(exitError{code: 2, output: []byte("usage")}) {
		t.Fatal("status 2 was treated as an index failure")
	}
	if !strings.Contains(err.Error(), "subprocess exited 1") {
		t.Fatalf("exit error text %q", err.Error())
	}
}
