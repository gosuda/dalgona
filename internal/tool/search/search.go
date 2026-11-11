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
	"os"
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
	if _, err := parseArgs(call.Args); err != nil {
		return tool.Result{}, err
	}
	id := call.ID
	if id == "" {
		var random [16]byte
		if _, err := rand.Read(random[:]); err != nil {
			return tool.Result{}, fmt.Errorf("search: allocate process id: %w", err)
		}
		id = "search-" + hex.EncodeToString(random[:])
	}
	spec := runner.ProcessSpec{
		ID: id, Argv: []string{"@self", "__search", string(call.Args)},
		Dir: env.Cwd, Sandbox: env.Sandbox,
	}
	if err := env.Runner.Start(ctx, spec); err != nil {
		return tool.Result{}, fmt.Errorf("search: start subprocess: %w", err)
	}
	data, err := collect(ctx, env.Runner, id)
	if err != nil {
		killCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), time.Second)
		defer cancel()
		if killErr := env.Runner.Signal(killCtx, id, runner.SignalKill); killErr != nil {
			return tool.Result{}, errors.Join(err, fmt.Errorf("search: kill subprocess: %w", killErr))
		}
		if waitErr := waitExit(env.Runner, id); waitErr != nil {
			return tool.Result{}, errors.Join(err, waitErr)
		}
		return tool.Result{}, err
	}
	result, err := decodeOutcome(data)
	if err != nil {
		return tool.Result{}, errors.New(string(data))
	}
	return makeResult(result, env.Snapshots), nil
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
				return nil, fmt.Errorf("search: subprocess exited %d: %s", out.Exit.Code, data)
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

const exitGrace = 5 * time.Second

func waitExit(run runner.Runner, id string) error {
	deadline := time.Now().Add(exitGrace)
	for {
		readCtx, cancel := context.WithTimeout(context.Background(), time.Second)
		state, readErr := run.Read(readCtx, id, 0)
		cancel()
		if readErr == nil && state.Exit != nil {
			return nil
		}
		if time.Now().After(deadline) {
			if readErr != nil {
				return fmt.Errorf("search: wait for subprocess exit: %w", readErr)
			}
			return errors.New("search: subprocess did not exit")
		}
		time.Sleep(20 * time.Millisecond)
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
	if len(argv) != 1 {
		_, _ = fmt.Fprint(stdout, "usage: __search <json-arguments>")
		return 2
	}
	parsed, err := parseArgs(jsontext.Value(argv[0]))
	if err != nil {
		_, _ = fmt.Fprint(stdout, err)
		return 2
	}
	cwd, err := os.Getwd()
	if err != nil {
		_, _ = fmt.Fprint(stdout, err)
		return 2
	}
	result, err := execute(cwd, parsed)
	if err != nil {
		_, _ = fmt.Fprint(stdout, err)
		return 2
	}
	if _, err := stdout.Write(encodeOutcome(result)); err != nil {
		return 1
	}
	return 0
}
