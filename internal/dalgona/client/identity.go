package client

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"os"
	"runtime"
	"syscall"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	MethodIdentity = "dalgona/daemon/identity"
	MethodRetire   = "dalgona/daemon/retire"

	UnknownPreIdentityBuild = "<unknown pre-identity build>"
	methodNotFound          = -32601

	socketGoneTimeout = 5 * time.Second
)

type Info struct {
	Build     string    `json:"build"`
	PID       int       `json:"pid"`
	Version   string    `json:"version"`
	StartedAt time.Time `json:"startedAt"`
}

type Probe int

const (
	ProbeAbsent Probe = iota
	ProbeReady
	ProbePreIdentity
)

var errUnidentified = errors.New("cannot safely identify the old daemon; stop it manually and retry dalgona restart")

type ReplacedError struct {
	Seen    Info
	Running Info
}

func (e *ReplacedError) Error() string {
	return fmt.Sprintf("daemon changed before restart: saw pid %d build %s, now pid %d build %s; reconnect and retry",
		e.Seen.PID, e.Seen.Build, e.Running.PID, e.Running.Build)
}

type BuildMismatchError struct {
	Running string
	Own     string
	Daemon  Info
}

func (e *BuildMismatchError) Error() string {
	return fmt.Sprintf("daemon runs build %s, this client is build %s; run dalgona restart", e.Running, e.Own)
}

type daemonClient struct{}

func (daemonClient) Notification(string, jsontext.Value) {}

func (daemonClient) Request(context.Context, string, jsontext.Value) (any, error) {
	return nil, errors.New("the daemon identity probe answers no server requests")
}

func ProbeDaemon(ctx context.Context, home string) (Info, Probe, error) {
	conn, err := dialPeer(SocketPath(home))
	if err != nil {
		return Info{}, ProbeAbsent, nil
	}
	return ProbeConn(ctx, conn)
}

func CheckBuild(ctx context.Context, home, own string) (Info, error) {
	info, probe, err := ProbeDaemon(ctx, home)
	if err != nil {
		return info, err
	}
	if probe == ProbePreIdentity {
		return info, &BuildMismatchError{Running: UnknownPreIdentityBuild, Own: own, Daemon: info}
	}
	if probe == ProbeReady && info.Build != "" && own != "" && info.Build != own {
		return info, &BuildMismatchError{Running: info.Build, Own: own, Daemon: info}
	}
	return info, nil
}

func Retire(ctx context.Context, home string, running Info) (bool, error) {
	conn, err := dialPeer(SocketPath(home))
	if err != nil {
		return false, nil
	}
	client := rpc.Dial(ctx, conn, daemonClient{})
	err = retireIdentified(ctx, client, running)
	_ = client.Close()
	_ = conn.Close()
	if err != nil {
		return false, err
	}
	if !waitSocketGone(ctx, home) {
		return false, errors.New("daemon did not stop after retire")
	}
	return true, nil
}

func retireIdentified(ctx context.Context, client *rpc.Conn, running Info) error {
	info, probe, err := identify(ctx, client)
	switch {
	case err != nil:
		return err
	case probe == ProbePreIdentity, !VerifiedPID(info.PID):
		return errUnidentified
	case !sameDaemon(running, info):
		return &ReplacedError{Seen: running, Running: info}
	}
	if err := client.Call(ctx, MethodRetire, nil, nil); err != nil && !errors.Is(err, io.ErrClosedPipe) {
		return err
	}
	return nil
}

func sameDaemon(a, b Info) bool {
	return a.PID == b.PID && a.Build == b.Build && a.StartedAt.Equal(b.StartedAt)
}

func waitSocketGone(ctx context.Context, home string) bool {
	sock := SocketPath(home)
	deadline := time.Now().Add(socketGoneTimeout)
	for {
		if _, err := os.Lstat(sock); errors.Is(err, os.ErrNotExist) {
			return true
		}
		if time.Now().After(deadline) {
			return false
		}
		timer := time.NewTimer(ensureRetry)
		select {
		case <-ctx.Done():
			timer.Stop()
			return false
		case <-timer.C:
		}
	}
}

func VerifiedPID(pid int) bool {
	if pid <= 0 {
		return false
	}
	if runtime.GOOS == "windows" {
		return true
	}
	proc, err := os.FindProcess(pid)
	if err != nil {
		return false
	}
	return proc.Signal(syscall.Signal(0)) == nil
}
