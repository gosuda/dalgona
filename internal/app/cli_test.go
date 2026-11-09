package app

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"io"
	"net"
	"reflect"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/rpc"
)

func TestTurnBookKeepsEarlyCompletionAndTexts(t *testing.T) {
	book := newTurnBook(&bytes.Buffer{})
	book.Notification("item/completed", jsontext.Value(`{"threadId":"t","turnId":"u1","completedAtMs":1,"item":{"id":"a","type":"agentMessage","text":"first"}}`))
	book.Notification("item/completed", jsontext.Value(`{"threadId":"t","turnId":"u2","completedAtMs":1,"item":{"id":"b","type":"agentMessage","text":"other turn"}}`))
	book.Notification("turn/completed", jsontext.Value(`{"threadId":"t","turn":{"id":"u1","items":[],"status":"completed"}}`))
	result, err := book.wait(t.Context(), "u1")
	if want := (turnResult{status: "completed", stop: "stop", texts: []string{"first"}}); err != nil || !reflect.DeepEqual(result, want) {
		t.Fatalf("early completion: %+v %v, want %+v", result, err, want)
	}
	book.parkNotify = make(chan struct{}, 1)
	waitCtx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	type lateOutcome struct {
		result turnResult
		err    error
	}
	done := make(chan lateOutcome, 1)
	go func() {
		late, waitErr := book.wait(waitCtx, "u2")
		done <- lateOutcome{result: late, err: waitErr}
	}()
	select {
	case <-book.parkNotify:
	case <-time.After(10 * time.Second):
		t.Fatal("turn-book waiter never parked on the changed channel")
	}
	book.Notification("turn/completed", jsontext.Value(`{"threadId":"t","turn":{"id":"u2","items":[],"status":"failed","error":{"message":"boom"}}}`))
	select {
	case outcome := <-done:
		if outcome.err != nil {
			t.Fatalf("late wait: %v", outcome.err)
		}
		if want := (turnResult{status: "failed", stop: "boom", texts: []string{"other turn"}}); !reflect.DeepEqual(outcome.result, want) {
			t.Fatalf("late completion: %+v, want %+v", outcome.result, want)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("late completion never arrived after the waiter parked")
	}
}

func TestStatusLineEncodesExactShape(t *testing.T) {
	line, err := statusLine("th\"read", turnResult{status: "interrupted", stop: "interrupted"})
	if err != nil {
		t.Fatal(err)
	}
	var decoded map[string]string
	if err := json.Unmarshal(line, &decoded); err != nil {
		t.Fatalf("%s: %v", line, err)
	}
	if len(decoded) != 3 || decoded["thread"] != "th\"read" || decoded["status"] != "interrupted" || decoded["stop_reason"] != "interrupted" {
		t.Fatalf("status line %s", line)
	}
}

func TestParseRejectsBadModes(t *testing.T) {
	for _, args := range [][]string{{"--approval", "never"}, {"--sandbox", "read-only"}, {"--resume", "x", "-c"}, {"stray"}} {
		var stderr bytes.Buffer
		env := CLIEnv{Product: "dal", Stderr: &stderr}
		if code := RunCLI(t.Context(), env, args); code != 2 {
			t.Fatalf("%v: exit %d, stderr %s", args, code, stderr.String())
		}
	}
}

type servedBox struct {
	home    string
	dials   int
	servers []net.Conn
	dial    func(context.Context) (io.ReadWriteCloser, error)
}

func newServedBox(t *testing.T) *servedBox {
	t.Helper()
	box := &servedBox{home: t.TempDir()}
	t.Setenv("DALGONA_HOME", box.home)
	host, closeHost, err := NewHost(t.Context(), Options{Product: config.ProductDalgona, Cwd: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = closeHost() })
	var served sync.WaitGroup
	t.Cleanup(served.Wait)
	box.dial = func(ctx context.Context) (io.ReadWriteCloser, error) {
		box.dials++
		client, server := net.Pipe()
		box.servers = append(box.servers, server)
		served.Go(func() { _ = rpc.Serve(context.WithoutCancel(ctx), server, host) })
		return client, nil
	}
	return box
}

func TestRemoteLoginAndLogoutUseTheServedHostStore(t *testing.T) {
	clientHome := t.TempDir()
	box := newServedBox(t)
	seedAPIKey(t, clientHome, "openai", "local-key")
	var stdout, stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDalgona, Home: clientHome, Remote: "box", Dial: box.dial, Stdout: &stdout, Stderr: &stderr}
	run := func(args ...string) int {
		stdout.Reset()
		stderr.Reset()
		return RunCLI(t.Context(), env, args)
	}
	checkRemoteLogin(t, run, &stderr, box.home, clientHome)
	checkRemoteLogout(t, run, &stderr, box.home, clientHome)
	checkRemoteDevinLoginRefused(t, run, &stderr, box)
}

func seedAPIKey(t *testing.T, home, provider, key string) {
	t.Helper()
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	if err := store.SetAPIKey(provider, key); err != nil {
		t.Fatal(err)
	}
}

func checkRemoteLogin(t *testing.T, run func(...string) int, stderr *bytes.Buffer, boxHome, clientHome string) {
	t.Helper()
	if code := run("login", "openai", "--api-key", "k"); code != 0 {
		t.Fatalf("remote login exit %d: %s", code, stderr.String())
	}
	if got := apiKeyIn(t, boxHome, "openai"); got != "k" {
		t.Fatalf("box openai key = %q, want k", got)
	}
	if got := apiKeyIn(t, clientHome, "openai"); got != "local-key" {
		t.Fatalf("client openai key = %q after remote login, want local-key", got)
	}
}

func checkRemoteLogout(t *testing.T, run func(...string) int, stderr *bytes.Buffer, boxHome, clientHome string) {
	t.Helper()
	if code := run("logout", "openai"); code != 0 {
		t.Fatalf("remote logout exit %d: %s", code, stderr.String())
	}
	if got := apiKeyIn(t, boxHome, "openai"); got != "" {
		t.Fatalf("box openai key = %q after remote logout", got)
	}
	if got := apiKeyIn(t, clientHome, "openai"); got != "local-key" {
		t.Fatalf("client openai key = %q after remote logout, want local-key", got)
	}
}

func checkRemoteDevinLoginRefused(t *testing.T, run func(...string) int, stderr *bytes.Buffer, box *servedBox) {
	t.Helper()
	before := box.dials
	if code := run("login", "devin"); code != 1 {
		t.Fatalf("remote devin login exit %d", code)
	}
	want := "dalgona: login devin cannot run inside remote box; run `dalgona login devin` here, then `dalgona cloud up box --copy-auth`\n"
	if stderr.String() != want || box.dials != before {
		t.Fatalf("remote devin login stderr %q after %d dials, want %q without a dial", stderr.String(), box.dials-before, want)
	}
}

func TestRemoteLoginWaitEndsOnInterruptAndOnLostConnection(t *testing.T) {
	box := newServedBox(t)
	env := CLIEnv{Product: config.ProductDalgona, Remote: "box", Dial: box.dial, Stderr: io.Discard}
	session, err := env.connect(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	defer session.close(env)
	interrupted, interrupt := context.WithCancel(t.Context())
	interrupt()
	if err := session.awaitLogin(interrupted, "login-missing"); !errors.Is(err, context.Canceled) || err.Error() != context.Canceled.Error() {
		t.Fatalf("interrupted wait = %v, want only context.Canceled after a delivered account/login/cancel", err)
	}
	done := make(chan error, 1)
	go func() { done <- session.awaitLogin(t.Context(), "login-pending") }()
	if err := box.servers[0].Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-done:
		if !errors.Is(err, errConnectionLost) {
			t.Fatalf("wait after the daemon closed = %v, want errConnectionLost", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("login wait did not end after the daemon connection closed")
	}
}

func apiKeyIn(t *testing.T, home, provider string) string {
	t.Helper()
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range store.Entries() {
		if entry.ID == provider {
			return entry.Credential.Key
		}
	}
	return ""
}
