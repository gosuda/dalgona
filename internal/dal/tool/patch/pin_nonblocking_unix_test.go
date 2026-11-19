//go:build unix

package patch

import (
	"context"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/tool"
	"golang.org/x/sys/unix"
)

const readBound = 5 * time.Second

func makeFifo(t *testing.T, path string) {
	t.Helper()
	if err := unix.Mkfifo(path, 0o600); err != nil {
		t.Fatal(err)
	}
}

func releaseBlockedReader(path string) {
	fd, err := unix.Open(path, unix.O_WRONLY|unix.O_NONBLOCK|unix.O_CLOEXEC, 0)
	if err == nil {
		_ = unix.Close(fd)
	}
}

func returnsWithin(t *testing.T, fifo, what string, run func()) {
	t.Helper()
	done := make(chan struct{})
	go func() {
		defer close(done)
		run()
	}()
	select {
	case <-done:
	case <-time.After(readBound):
		releaseBlockedReader(fifo)
		<-done
		t.Fatalf("%s did not return within %s: a named pipe without a writer blocked the read", what, readBound)
	}
}

type leafReader interface {
	readFileAt(base string) ([]byte, error)
	close()
}

type leafReaderKind struct {
	name string
	pin  func(env tool.Env, display, absolute string) (leafReader, error)
}

var leafReaderKinds = []leafReaderKind{
	{"dirPin", func(env tool.Env, display, absolute string) (leafReader, error) {
		return pinVerifiedComponents(env, display, absolute)
	}},
	{"rootPin", func(env tool.Env, display, absolute string) (leafReader, error) {
		return pinRoot(env, display, absolute)
	}},
}

type leafCase struct {
	name   string
	leaf   string
	want   string
	err    error
	absent bool
}

var leafCases = []leafCase{
	{"IO-03/regular file", "regular.txt", "bytes\n", nil, false},
	{"IO-04/absent leaf", "absent.txt", "", fs.ErrNotExist, true},
	{"IO-04/named pipe", "pipe", "", errNotRegular, false},
	{"IO-04/directory", "dir", "", errNotRegular, false},
}

func leafFixture(t *testing.T) (tool.Env, string, string) {
	t.Helper()
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "sub/regular.txt", "bytes\n")
	if err := os.Mkdir(filepath.Join(cwd, "sub", "dir"), 0o755); err != nil {
		t.Fatal(err)
	}
	fifo := filepath.Join(cwd, "sub", "pipe")
	makeFifo(t, fifo)
	return env, cwd, fifo
}

func TestPinnedReadAcceptsOnlyRegularFiles(t *testing.T) {
	for _, kind := range leafReaderKinds {
		for _, test := range leafCases {
			t.Run(kind.name+"/"+test.name, func(t *testing.T) {
				requirePinnedRead(t, kind, test)
			})
		}
	}
}

func requirePinnedRead(t *testing.T, kind leafReaderKind, test leafCase) {
	t.Helper()
	env, cwd, fifo := leafFixture(t)
	pin, err := kind.pin(env, "sub/"+test.leaf, filepath.Join(cwd, "sub", test.leaf))
	if err != nil {
		t.Fatalf("pin %s: %v", test.leaf, err)
	}
	defer pin.close()
	var data []byte
	returnsWithin(t, fifo, "readFileAt", func() { data, err = pin.readFileAt(test.leaf) })
	if !errors.Is(err, test.err) {
		t.Fatalf("readFileAt(%s) error = %v, want %v", test.leaf, err, test.err)
	}
	if errors.Is(err, fs.ErrNotExist) != test.absent {
		t.Fatalf("readFileAt(%s) reports absence = %v, want %v: %v", test.leaf, !test.absent, test.absent, err)
	}
	if string(data) != test.want {
		t.Fatalf("readFileAt(%s) = %q, want %q", test.leaf, data, test.want)
	}
}

type occupyOnStageContext struct {
	context.Context
	dir    string
	occupy func(dir string) error
	fired  bool
	err    error
}

func (c *occupyOnStageContext) Err() error {
	if c.fired {
		return nil
	}
	temps, err := filepath.Glob(filepath.Join(c.dir, "*.dalgona-tmp-*"))
	if err == nil && len(temps) > 0 {
		c.fired = true
		c.err = c.occupy(c.dir)
	}
	return nil
}

const occupiedCreateInput = "[a.txt#NEW]\nPUT >$:\n+a\n[build/events#NEW]\nPUT >$:\n+x\n"

type occupant struct {
	name   string
	occupy func(path string) error
	want   tool.Outcome
	kind   fs.FileMode
	body   string
}

var occupants = []occupant{
	{
		"IO-04 C13/named pipe without a writer",
		func(path string) error { return unix.Mkfifo(path, 0o600) },
		withPaths(commitUnknown, "a.txt", "before", "build/events", "unknown"),
		fs.ModeNamedPipe,
		"",
	},
	{
		"IO-03 C13/regular file holding the staged bytes",
		func(path string) error { return os.WriteFile(path, []byte("x"), 0o644) },
		withPaths(writeFailure(tool.CommitPartial, tool.ReasonSyncFailed), "a.txt", "before", "build/events", "after"),
		0,
		"x",
	},
}

func TestCommitFailureOverAnOccupiedCreateTargetStaysBounded(t *testing.T) {
	for _, style := range profileStyles {
		for _, test := range occupants {
			t.Run(test.name+"/"+string(style), func(t *testing.T) {
				requireOccupiedCommit(t, style, test)
			})
		}
	}
}

func requireOccupiedCommit(t *testing.T, style Style, test occupant) {
	t.Helper()
	f := newOutcomeFixture(t)
	plan, err := f.plan(t, style, occupiedCreateInput)
	if err != nil {
		t.Fatal(err)
	}
	build := filepath.Join(f.cwd, "build")
	ctx := &occupyOnStageContext{Context: t.Context(), dir: build, occupy: func(dir string) error {
		return test.occupy(filepath.Join(dir, "events"))
	}}
	var applyErr error
	returnsWithin(t, filepath.Join(build, "events"), "Apply", func() { _, applyErr = plan.Apply(ctx) })
	if !ctx.fired || ctx.err != nil {
		t.Fatalf("the target was not occupied during staging: fired=%v err=%v", ctx.fired, ctx.err)
	}
	if applyErr == nil {
		t.Fatal("the batch succeeded over an occupied path")
	}
	requireWrappedOutcome(t, applyErr, style, test.want)
	requireOccupant(t, filepath.Join(build, "events"), test)
	requireMissing(t, filepath.Join(f.cwd, "a.txt"))
	requireNoStagingResidue(t, build)
}

func requireOccupant(t *testing.T, path string, test occupant) {
	t.Helper()
	info, err := os.Lstat(path)
	if err != nil {
		t.Fatalf("the occupant was removed: %v", err)
	}
	if info.Mode().Type() != test.kind {
		t.Fatalf("occupant type %v, want %v", info.Mode().Type(), test.kind)
	}
	if test.body != "" {
		requireFile(t, path, test.body)
	}
}

func requireNoStagingResidue(t *testing.T, dir string) {
	t.Helper()
	residue, err := filepath.Glob(filepath.Join(dir, "*.dalgona-*"))
	if err != nil || len(residue) != 0 {
		t.Fatalf("patch residue remains: %v, %v", residue, err)
	}
}
