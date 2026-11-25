package client

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
	"time"
	"unicode"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func sshArgs(destination, command string) []string {
	return []string{
		"-T", "-oBatchMode=yes", "-oStrictHostKeyChecking=yes", "-oUpdateHostKeys=no",
		"-oNoHostAuthenticationForLocalhost=no", "-oVerifyHostKeyDNS=no",
		"-oForwardAgent=no", "-oForwardX11=no", "-oClearAllForwardings=yes", "-oPermitLocalCommand=no",
		"-oRemoteCommand=none", "-oControlMaster=no", "-oControlPath=none", "-oStdinNull=no",
		"--", destination, command,
	}
}

type sshStream struct {
	input   *os.File
	output  *os.File
	process *os.Process
	done    chan struct{}
	close   sync.Once
	stderr  *sshDiagnostic
}

func (s *sshStream) Read(p []byte) (int, error)  { return s.output.Read(p) }
func (s *sshStream) Write(p []byte) (int, error) { return s.input.Write(p) }
func (s *sshStream) CloseWrite() error           { return s.input.Close() }

func (s *sshStream) Close() error {
	s.close.Do(func() {
		_ = s.input.Close()
		_ = s.output.Close()
		_ = s.process.Kill()
		<-s.done
	})
	return nil
}

func startSSH(cmd *exec.Cmd) (*sshStream, error) {
	fromClient, input, err := os.Pipe()
	if err != nil {
		return nil, fmt.Errorf("open SSH stdin pipe: %w", err)
	}
	defer func() { _ = fromClient.Close() }()
	output, toClient, err := os.Pipe()
	if err != nil {
		_ = input.Close()
		return nil, fmt.Errorf("open SSH stdout pipe: %w", err)
	}
	defer func() { _ = toClient.Close() }()
	stderr := &sshDiagnostic{}
	cmd.Stdin, cmd.Stdout, cmd.Stderr = fromClient, toClient, stderr
	cmd.WaitDelay = 2 * time.Second
	if err := cmd.Start(); err != nil {
		_ = input.Close()
		_ = output.Close()
		return nil, fmt.Errorf("start OpenSSH: %w", err)
	}
	done := make(chan struct{})
	go func() {
		_ = cmd.Wait()
		close(done)
	}()
	return &sshStream{input: input, output: output, process: cmd.Process, done: done, stderr: stderr}, nil
}

func dialSSH(ctx context.Context, spec config.RemoteSpec) (*sshStream, string, error) {
	if !config.ValidSSHDestination(spec.SSHDestination) {
		return nil, "", refusal{errors.New("invalid SSH destination")}
	}
	command, err := sshBridgeCommand(spec)
	if err != nil {
		return nil, "", refusal{err}
	}
	stream, err := startSSH(exec.Command("ssh", sshArgs(spec.SSHDestination, command)...))
	if err != nil {
		return nil, "", refusal{err}
	}
	id, err := sshIdentify(ctx, stream, spec.MachineID)
	if err != nil {
		_ = stream.Close()
		if detail := stream.stderr.String(); detail != "" {
			err = fmt.Errorf("%w: %s", err, detail)
		}
		return nil, "", err
	}
	return stream, id, nil
}

func sshIdentify(ctx context.Context, stream *sshStream, expected string) (string, error) {
	stop := context.AfterFunc(ctx, func() { _ = stream.Close() })
	var reply rpc.MachineReply
	err := rpc.ReadMachineFrame(stream, rpc.MachinePrefaceLimit, &reply)
	if !stop() || ctx.Err() != nil {
		return "", context.Cause(ctx)
	}
	if err != nil {
		return "", fmt.Errorf("SSH bridge did not report its daemon identity: %w", err)
	}
	if reply.Version != rpc.MachineVersion {
		return "", refusal{errors.New("SSH bridge did not report a compatible daemon identity; update the remote CLI")}
	}
	if err := machineReplyError(expected, reply); err != nil {
		return "", refusal{err}
	}
	if !config.ValidMachineDigest(reply.MachineID) {
		return "", refusal{errors.New("SSH bridge did not report a valid daemon identity")}
	}
	if expected != "" && reply.MachineID != expected {
		return "", refusal{&MachineMismatchError{Expected: expected, Actual: reply.MachineID}}
	}
	return reply.MachineID, nil
}

type sshDiagnostic struct {
	mu  sync.Mutex
	buf []byte
}

func (d *sshDiagnostic) Write(p []byte) (int, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	n := min(len(p), 4096-len(d.buf))
	d.buf = append(d.buf, p[:n]...)
	return len(p), nil
}

func (d *sshDiagnostic) String() string {
	d.mu.Lock()
	defer d.mu.Unlock()
	return strings.TrimSpace(strings.Map(func(r rune) rune {
		if unicode.IsControl(r) {
			return ' '
		}
		return r
	}, string(d.buf)))
}

var _ io.ReadWriteCloser = (*sshStream)(nil)
