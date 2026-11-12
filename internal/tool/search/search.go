package search

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type Tool struct{}

func New() tool.Tool { return Tool{} }

func (Tool) Spec() tool.Spec {
	return tool.Spec{
		Name:        "search",
		Description: "Search the workspace. mode \"find\" lists files whose path matches the glob in pattern. A glob without / matches the file name at any depth. mode \"grep\" lists lines that match pattern, an RE2 regular expression, or plain text when literal is true. Search skips .git and every path that .gitignore or .git/info/exclude excludes. It does not follow symbolic links. It skips binary files and files over 16 MiB. find returns at most limit paths, sorted. grep returns at most limit matches as path:line:text and cuts lines at 500 characters. When results were cut, the last line starts with [Truncated:.",
		Schema:      jsontext.Value(`{"type":"object","properties":{"mode":{"type":"string","enum":["find","grep"]},"pattern":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string"},"literal":{"type":"boolean"},"ignore_case":{"type":"boolean"},"limit":{"type":"integer","minimum":1,"maximum":1000}},"required":["mode","pattern"],"additionalProperties":false}`),
		Keep:        tool.KeepHead,
	}
}

func (Tool) Class() tool.Class { return tool.ClassRead }

func (Tool) Run(ctx context.Context, call tool.Call, env tool.Env) (tool.Result, error) {
	parsed, err := parseArgs(call.Args)
	if err != nil {
		return tool.Result{}, err
	}
	id := call.ID
	if id == "" {
		if id, err = newProcessID("search-"); err != nil {
			return tool.Result{}, err
		}
	}
	var result outcome
	if parsed.Mode == "grep" {
		result, err = runGrep(ctx, env, id, call.Args)
	} else {
		result, err = runSearch(ctx, env, id, call.Args, indexRef{})
	}
	if err != nil {
		return tool.Result{}, err
	}
	return makeResult(result, env.Snapshots), nil
}

type exitError struct {
	code   int
	output []byte
}

func (e exitError) Error() string {
	return fmt.Sprintf("search: subprocess exited %d: %s", e.code, e.output)
}

func newProcessID(prefix string) (string, error) {
	var random [16]byte
	if _, err := rand.Read(random[:]); err != nil {
		return "", fmt.Errorf("search: allocate process id: %w", err)
	}
	return prefix + hex.EncodeToString(random[:]), nil
}

func searchArgs(raw jsontext.Value, ref indexRef, now time.Time) []string {
	argv := []string{"@self", "__search", string(raw)}
	if ref.gen == "" {
		return argv
	}
	return append(argv, ref.root, ref.gen, strconv.FormatInt(now.UnixNano(), 10), strconv.FormatInt(racyWindow.Nanoseconds(), 10))
}

func runSearch(ctx context.Context, env tool.Env, id string, raw jsontext.Value, ref indexRef) (outcome, error) {
	spec := runner.ProcessSpec{
		ID: id, Argv: searchArgs(raw, ref, time.Now()),
		Dir: env.Cwd, Sandbox: env.Sandbox,
	}
	if err := env.Runner.Start(ctx, spec); err != nil {
		return outcome{}, fmt.Errorf("search: start subprocess: %w", err)
	}
	data, err := collect(ctx, env.Runner, id)
	if err != nil {
		return outcome{}, killAndAwait(ctx, env.Runner, id, err)
	}
	result, err := decodeOutcome(data)
	if err != nil {
		return outcome{}, errors.New(string(data))
	}
	return result, nil
}

func killAndAwait(ctx context.Context, run runner.Runner, id string, cause error) error {
	killCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), time.Second)
	defer cancel()
	if killErr := run.Signal(killCtx, id, runner.SignalKill); killErr != nil {
		return errors.Join(cause, fmt.Errorf("search: kill subprocess: %w", killErr))
	}
	if waitErr := runner.AwaitExit(ctx, run, id, 5*time.Second); waitErr != nil {
		return errors.Join(cause, fmt.Errorf("search: wait for subprocess exit: %w", waitErr))
	}
	return cause
}

func runGrep(ctx context.Context, env tool.Env, id string, raw jsontext.Value) (outcome, error) {
	return grepWithIndex(ctx, env, indexFor(env), id, raw)
}

func grepWithIndex(ctx context.Context, env tool.Env, ref indexRef, id string, raw jsontext.Value) (outcome, error) {
	result, err := runSearch(ctx, env, id, raw, ref)
	if err != nil && ref.gen != "" && isIndexFailure(err) {
		result, err = retryWithoutIndex(ctx, env, ref, raw, err)
	}
	if err == nil && ref.root != "" && int64(result.Unindexed) >= indexRebuildBytes {
		startBuild(ref.root, env)
	}
	return result, err
}

func retryWithoutIndex(ctx context.Context, env tool.Env, ref indexRef, raw jsontext.Value, cause error) (outcome, error) {
	slog.Default().Warn("search: index failed, retrying without it", "root", ref.root, "generation", ref.gen, "error", cause)
	if err := forgetGeneration(ref.root, ref.gen); err != nil {
		slog.Default().Warn("search: could not remove the failed index", "root", ref.root, "generation", ref.gen, "error", err)
	}
	if err := recordIndexFailure(ref.root, time.Now()); err != nil {
		slog.Default().Warn("search: could not record the index failure", "root", ref.root, "error", err)
	}
	id, err := newProcessID("search-")
	if err != nil {
		return outcome{}, err
	}
	return runSearch(ctx, env, id, raw, indexRef{})
}

const corruptIndexText = "corrupt index"

func isIndexFailure(err error) bool {
	exit, ok := errors.AsType[exitError](err)
	if !ok || exit.code != indexFailedStatus {
		return false
	}
	return bytes.Contains(exit.output, []byte(errIndex.Error())) || bytes.Contains(exit.output, []byte(corruptIndexText))
}

func collect(ctx context.Context, run runner.Runner, id string) ([]byte, error) {
	var data []byte
	var offset int64
	for {
		out, err := run.Read(ctx, id, offset)
		if err != nil {
			return nil, fmt.Errorf("search: read subprocess: %w", err)
		}
		data = append(data, out.Data...)
		offset = out.Next
		if out.Exit != nil {
			if out.Exit.Code != 0 {
				return nil, exitError{code: out.Exit.Code, output: data}
			}
			return data, nil
		}
		if !out.Alive {
			return nil, errors.New("search: subprocess stopped without an exit record")
		}
		timer := time.NewTimer(5 * time.Millisecond)
		select {
		case <-ctx.Done():
			timer.Stop()
			return nil, ctx.Err()
		case <-timer.C:
		}
	}
}

func makeResult(result outcome, store *snapshot.Store) tool.Result {
	const maxMetaBytes = 64 << 10
	meta := []byte(`{"captures":[`)
	var output strings.Builder
	output.Grow(len(result.Text) + 1 + len(result.Captures)*128)
	output.WriteString(result.Text)
	if result.Text != "" && !strings.HasSuffix(result.Text, "\n") {
		output.WriteByte('\n')
	}
	for _, item := range result.Captures {
		entry, ok := store.Capture(item.Path, item.Data, item.Shown)
		if !ok {
			continue
		}
		encoded := entry.Meta()
		if len(meta)+len(encoded)+3 > maxMetaBytes {
			break
		}
		meta = append(meta, encoded...)
		meta = append(meta, ',')
		fmt.Fprintf(&output, "[%s#%s]\n[%s#%s]\n[%s@%s]\n",
			item.Path, snapshot.Tag("whole", item.Data),
			item.Path, snapshot.VersionTag(item.Data),
			snapshot.HeaderPath(item.Path), entry.Ref)
	}
	meta = append(bytes.TrimSuffix(meta, []byte{','}), `]}`...)
	return tool.Result{Output: []byte(output.String()), OK: true, Meta: jsontext.Value(meta)}
}

func Main(argv []string, stdout io.Writer) int {
	if len(argv) != 1 && len(argv) != 5 {
		_, _ = fmt.Fprint(stdout, "usage: __search <json-arguments> [<root> <generation> <now-ns> <racy-ns>]")
		return 2
	}
	parsed, err := parseArgs(jsontext.Value(argv[0]))
	if err != nil {
		_, _ = fmt.Fprint(stdout, err)
		return 2
	}
	dir, clock, err := parseIndexArgs(argv[1:])
	if err != nil {
		_, _ = fmt.Fprint(stdout, err)
		return 2
	}
	cwd, err := os.Getwd()
	if err != nil {
		_, _ = fmt.Fprint(stdout, err)
		return 2
	}
	result, err := execute(cwd, parsed, dir, clock)
	if err != nil {
		_, _ = fmt.Fprint(stdout, err)
		if errors.Is(err, errIndex) {
			return indexFailedStatus
		}
		return 2
	}
	if _, err := stdout.Write(encodeOutcome(result)); err != nil {
		return 2
	}
	return 0
}
