package dalgonawire

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"io"
	"net"
	"os"
	"os/signal"
	"strings"
	"sync"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/tui"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

func TestTerminalMachinesIsolateDelayedWorkAndCollidingDecisions(t *testing.T) {
	t.Parallel()
	signals := make(chan os.Signal, 1)
	signal.Notify(signals, syscall.SIGINT)
	signal.Stop(signals)
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithTimeout(t.Context(), 20*time.Second)
		defer func() {
			cancel()
			time.Sleep(time.Minute)
			synctest.Wait()
		}()
		east, west := newWireMachine("East"), newWireMachine("West")
		rig := launchMachineTest(ctx, east, west)
		defer rig.keys.Close()
		defer east.releaseShell()
		rig.output.waitFor(t, ctx, "started East thread")
		assertPrivatePairingEntry(t, ctx, rig)
		eastConn := receive(t, ctx, east.dialed)
		rig.typeKeys(t, "!echo delayed\r")
		if cwd := receive(t, ctx, east.shellCwd); cwd != "/East" {
			t.Fatalf("shell cwd = %q, want /East", cwd)
		}
		rig.typeKeys(t, "east-draft\x07")
		rig.output.waitFor(t, ctx, "Machines", "West · idle")
		rig.typeKeys(t, "\x1b[Bc")
		rig.output.waitFor(t, ctx, "West · connected")
		westConn := receive(t, ctx, west.dialed)
		rig.typeKeys(t, "\r")
		rig.output.waitFor(t, ctx, "── West ──")
		rig.typeKeys(t, "/resume same-thread\r")
		waitOnMachine(t, ctx, rig.status, "West", "same-thread")
		assertMachineDecisions(t, ctx, rig, east, [2]*rpc.Conn{eastConn, westConn})
		fresh := assertMachineReconnect(t, ctx, rig, east, eastConn)
		cancel()
		exit, stop := context.WithTimeout(t.Context(), time.Second)
		defer stop()
		if err := receive(t, exit, rig.result); err != nil {
			t.Fatalf("terminal exit: %v", err)
		}
		receive(t, exit, fresh.Done())
		receive(t, exit, westConn.Done())
	})
}

func assertPrivatePairingEntry(t *testing.T, ctx context.Context, rig machineTestRun) {
	t.Helper()
	rig.typeKeys(t, "\x07")
	rig.output.waitFor(t, ctx, "Machines")
	rig.typeKeys(t, "p")
	for _, field := range []struct{ label, value string }{
		{"Machine name", "unused-pair"},
		{"HTTPS origin", "https://pairing.invalid"},
		{"Machine ID", "synthetic-machine-id"},
		{"TLS public-key pin", "synthetic-public-key-pin"},
	} {
		rig.output.waitFor(t, ctx, field.label)
		rig.typeKeys(t, field.value+"\r")
	}
	const code = "private-pairing-proof"
	rig.output.waitFor(t, ctx, "Pairing code")
	rig.typeKeys(t, code)
	rig.output.waitFor(t, ctx, strings.Repeat("*", len(code)))
	rig.typeKeys(t, "\r")
	rig.output.waitFor(t, ctx, "Device label")
	rig.typeKeys(t, "Test device\r")
	rig.output.waitFor(t, ctx, "FULL access to the daemon account", "trusted SSH into that account")
	rig.typeKeys(t, "\x07")
	if output := rig.output.String(); strings.Contains(output, code) {
		t.Fatalf("pairing code appeared in terminal output:\n%s", strings.ReplaceAll(output, code, "[redacted]"))
	}
}

func assertMachineDecisions(t *testing.T, ctx context.Context, rig machineTestRun, east *wireMachine, conns [2]*rpc.Conn) {
	t.Helper()
	eastAnswer := requestDecision(ctx, conns[0], "East approval")
	westAnswer := requestDecision(ctx, conns[1], "West approval")
	rig.output.waitFor(t, ctx, "West approval")
	east.releaseShell()
	synctest.Wait()
	time.Sleep(100 * time.Millisecond)
	if output := rig.output.String(); strings.Contains(output, "East approval") || strings.Contains(output, "isolated-shell-output") {
		t.Fatalf("East work surfaced while West was selected:\n%s", output)
	}
	rig.typeKeys(t, "y")
	if result := receive(t, ctx, westAnswer); result.err != nil || result.decision != "accept" {
		t.Fatalf("West decision = %+v, want accept", result)
	}
	select {
	case result := <-eastAnswer:
		t.Fatalf("West’s answer resolved East’s colliding request: %+v", result)
	default:
	}
	rig.typeKeys(t, "\x07")
	rig.output.waitFor(t, ctx, "West · connected · selected")
	switchAt := len(rig.output.String())
	rig.typeKeys(t, "\x1b[A\r")
	rig.output.waitAfter(t, ctx, switchAt, "East approval", "isolated-shell-output", "east-draft", "── East ──")
	rig.typeKeys(t, "n")
	if result := receive(t, ctx, eastAnswer); result.err != nil || result.decision != "decline" {
		t.Fatalf("East decision = %+v, want decline", result)
	}
}

func assertMachineReconnect(t *testing.T, ctx context.Context, rig machineTestRun, east *wireMachine, eastConn *rpc.Conn) *rpc.Conn {
	t.Helper()
	old := requestDecision(ctx, eastConn, "Decision on lost link")
	rig.output.waitFor(t, ctx, "Decision on lost link")
	if err := eastConn.Close(); err != nil {
		t.Fatal(err)
	}
	if result := receive(t, ctx, old); result.err == nil {
		t.Fatalf("lost-link decision completed: %+v", result)
	}
	fresh := receive(t, ctx, east.dialed)
	receive(t, ctx, east.resumed)
	freshAnswer := requestDecision(ctx, fresh, "Decision on fresh link")
	rig.output.waitFor(t, ctx, "Decision on fresh link")
	rig.typeKeys(t, "y")
	if result := receive(t, ctx, freshAnswer); result.err != nil || result.decision != "accept" {
		t.Fatalf("fresh-link decision = %+v, want accept", result)
	}
	return fresh
}

type decision struct {
	decision string
	err      error
}

func requestDecision(ctx context.Context, conn *rpc.Conn, command string) <-chan decision {
	answered := make(chan decision, 1)
	go func() {
		params := rpc.CommandExecutionRequestApprovalParams{ThreadID: "same-thread", TurnID: "same-turn", ItemID: "same-item", Command: command}
		var result rpc.CommandExecutionRequestApprovalResponse
		err := conn.CallID(ctx, "same-request", "item/commandExecution/requestApproval", params, &result)
		answered <- decision{decision: result.Decision, err: err}
	}()
	return answered
}

type wireMachine struct {
	name        string
	dialed      chan *rpc.Conn
	resumed     chan struct{}
	shellCwd    chan string
	shellResult chan struct{}
	unblock     func()
}

func newWireMachine(name string) *wireMachine {
	shell := make(chan struct{})
	return &wireMachine{
		name: name, dialed: make(chan *rpc.Conn, 2), resumed: make(chan struct{}, 2),
		shellCwd: make(chan string, 1), shellResult: shell, unblock: sync.OnceFunc(func() { close(shell) }),
	}
}

func (m *wireMachine) dial(context.Context) (io.ReadWriteCloser, error) {
	near, far := net.Pipe()
	m.dialed <- rpc.NewConn(context.Background(), far, m.handle)
	return near, nil
}

func (m *wireMachine) releaseShell() { m.unblock() }

func (m *wireMachine) handle(ctx context.Context, method string, params jsontext.Value) (jsontext.Value, error) {
	switch method {
	case "initialize":
		return json.Marshal(rpc.InitializeResult{UserAgent: "dalgona/test", PlatformFamily: "unix"})
	case "initialized", "dalgona/settings/read", "model/list":
		return jsontext.Value(`{}`), nil
	case "dalgona/keywords/list", "dalgona/keywords/match":
		return jsontext.Value(`[]`), nil
	case "thread/start", "thread/resume":
		if method == "thread/resume" {
			m.resumed <- struct{}{}
		}
		thread := rpc.Thread{ID: "same-thread", Cwd: "/" + m.name, Name: m.name + " thread", Status: rpc.ThreadStatus{Type: "idle"}}
		return json.Marshal(rpc.ThreadResumeResponse{Thread: thread, Cwd: thread.Cwd, Model: "test", ApprovalPolicy: "on-request"})
	case "command/exec":
		return m.shell(ctx, params)
	}
	return nil, &rpc.RPCError{Code: -32601, Message: "Unexpected method " + method}
}

func (m *wireMachine) shell(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var call struct {
		Cwd string `json:"cwd"`
	}
	if err := json.Unmarshal(params, &call); err != nil {
		return nil, err
	}
	m.shellCwd <- call.Cwd
	select {
	case <-m.shellResult:
		return jsontext.Value(`{"exitCode":0,"stdout":"isolated-shell-output"}`), nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

type machineTestRun struct {
	keys   *io.PipeWriter
	output *testTerminal
	status <-chan tui.Status
	result <-chan error
}

func launchMachineTest(ctx context.Context, east, west *wireMachine) machineTestRun {
	input, keys := io.Pipe()
	output := &testTerminal{changed: make(chan struct{}, 1)}
	status := make(chan tui.Status, 128)
	result := make(chan error, 1)
	open := func(events tui.MachineEvents) (tui.Machines, error) {
		specs := []client.EndpointSpec{
			{ID: "East", Label: "East", Kind: client.EndpointLocal, Dial: east.dial},
			{ID: "West", Label: "West", Kind: client.EndpointDaemon, Dial: west.dial},
		}
		set, err := client.NewEndpointSet(nil, specs, observer{events: events}, client.ProtocolTUI)
		return machines{set: set}, err
	}
	go func() {
		defer input.Close()
		opts := tui.Options{Input: input, Output: output, Width: 96, Height: 30, OnStatus: func(s tui.Status) { status <- s }}
		result <- tui.Run(ctx, open, opts)
	}()
	return machineTestRun{keys: keys, output: output, status: status, result: result}
}

func (r machineTestRun) typeKeys(t *testing.T, keys string) {
	t.Helper()
	if _, err := io.WriteString(r.keys, keys); err != nil {
		t.Fatal(err)
	}
}

func waitOnMachine(t *testing.T, ctx context.Context, statuses <-chan tui.Status, name, id string) {
	t.Helper()
	for {
		if status := receive(t, ctx, statuses); status.Endpoint == name && status.ThreadID == id {
			return
		}
	}
}

func receive[T any](t *testing.T, ctx context.Context, ch <-chan T) T {
	t.Helper()
	select {
	case msg := <-ch:
		return msg
	case <-ctx.Done():
		t.Fatal(ctx.Err())
		var zero T
		return zero
	}
}

type testTerminal struct {
	mu      sync.Mutex
	data    bytes.Buffer
	changed chan struct{}
}

func (b *testTerminal) Write(data []byte) (int, error) {
	b.mu.Lock()
	n, err := b.data.Write(data)
	b.mu.Unlock()
	select {
	case b.changed <- struct{}{}:
	default:
	}
	return n, err
}

func (b *testTerminal) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.data.String()
}

func (b *testTerminal) waitFor(t *testing.T, ctx context.Context, wants ...string) {
	t.Helper()
	b.waitAfter(t, ctx, 0, wants...)
}

func (b *testTerminal) waitAfter(t *testing.T, ctx context.Context, offset int, wants ...string) {
	t.Helper()
	for _, want := range wants {
		for !strings.Contains(b.String()[offset:], want) {
			select {
			case <-b.changed:
			case <-ctx.Done():
				t.Fatalf("terminal never rendered %q: %v\n%s", want, ctx.Err(), b.String()[offset:])
			}
		}
	}
}
