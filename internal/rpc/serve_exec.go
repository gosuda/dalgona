package rpc

import (
	"cmp"
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	"fmt"
	"path/filepath"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/config"
)

const (
	execDefaultTimeoutMS = 600000
	execDefaultOutputCap = 1 << 20
	execMaxOutputCap     = 64 << 20
	execMaxProcesses     = 16
)

type CommandExecResponse struct {
	ExitCode int    `json:"exitCode"`
	Stdout   string `json:"stdout"`
	Stderr   string `json:"stderr"`
}

func (r CommandExecResponse) write(o *out) {
	o.open()
	o.int("exitCode", int64(r.ExitCode))
	o.str("stderr", r.Stderr)
	o.str("stdout", r.Stdout)
	o.close()
}

type commandOutputDelta struct {
	processID  string
	data       []byte
	capReached bool
}

func (d commandOutputDelta) write(o *out) {
	o.open()
	o.str("processId", d.processID)
	o.str("stream", "stdout")
	o.str("deltaBase64", base64.StdEncoding.EncodeToString(d.data))
	o.boolean("capReached", d.capReached)
	o.close()
}

type execParams struct {
	command            []string
	cwd                string
	env                map[string]*string
	timeoutMS          int
	hasTimeout         bool
	disableTimeout     bool
	outputCap          int
	hasOutputCap       bool
	disableOutputCap   bool
	processID          string
	streamStdoutStderr bool
	tty                bool
	streamStdin        bool
	size               bool
	sandboxPolicy      string
}

type execMember func(p *execParams, d *jsontext.Decoder) error

var execMembers = map[string]execMember{
	"command":            readExecCommand,
	"cwd":                readExecString(func(p *execParams) *string { return &p.cwd }),
	"processId":          readExecString(func(p *execParams) *string { return &p.processID }),
	"env":                readExecEnv,
	"timeoutMs":          readExecLimit(func(p *execParams) (*int, *bool) { return &p.timeoutMS, &p.hasTimeout }),
	"outputBytesCap":     readExecLimit(func(p *execParams) (*int, *bool) { return &p.outputCap, &p.hasOutputCap }),
	"disableTimeout":     readExecBool(func(p *execParams) *bool { return &p.disableTimeout }),
	"disableOutputCap":   readExecBool(func(p *execParams) *bool { return &p.disableOutputCap }),
	"streamStdoutStderr": readExecBool(func(p *execParams) *bool { return &p.streamStdoutStderr }),
	"tty":                readExecBool(func(p *execParams) *bool { return &p.tty }),
	"streamStdin":        readExecBool(func(p *execParams) *bool { return &p.streamStdin }),
	"size":               readExecSize,
	"sandboxPolicy":      readExecSandboxPolicy,
}

func (p *execParams) member(name string, d *jsontext.Decoder) error {
	if read, ok := execMembers[name]; ok {
		return read(p, d)
	}
	return d.SkipValue()
}

func readExecCommand(p *execParams, d *jsontext.Decoder) error {
	return readArray(d, func() error {
		v, err := readString(d)
		p.command = append(p.command, v)
		return err
	})
}

func readExecString(field func(p *execParams) *string) execMember {
	return func(p *execParams, d *jsontext.Decoder) error {
		v, err := readNullableString(d)
		*field(p) = v
		return err
	}
}

func readExecBool(field func(p *execParams) *bool) execMember {
	return func(p *execParams, d *jsontext.Decoder) error {
		if null, err := consumeNull(d); null || err != nil {
			return err
		}
		v, err := readBool(d)
		*field(p) = v
		return err
	}
}

func readExecLimit(field func(p *execParams) (*int, *bool)) execMember {
	return func(p *execParams, d *jsontext.Decoder) error {
		if null, err := consumeNull(d); null || err != nil {
			return err
		}
		v, err := readInt(d)
		value, set := field(p)
		*value, *set = v, true
		return err
	}
}

func readExecSize(p *execParams, d *jsontext.Decoder) error {
	if null, err := consumeNull(d); null || err != nil {
		return err
	}
	p.size = true
	return d.SkipValue()
}

func readExecEnv(p *execParams, d *jsontext.Decoder) error {
	if null, err := consumeNull(d); null || err != nil {
		return err
	}
	p.env = map[string]*string{}
	return readMembers(d, func(name string, d *jsontext.Decoder) error {
		null, err := consumeNull(d)
		if err != nil {
			return err
		}
		if null {
			p.env[name] = nil
			return nil
		}
		v, err := readString(d)
		p.env[name] = &v
		return err
	})
}

func readExecSandboxPolicy(p *execParams, d *jsontext.Decoder) error {
	if null, err := consumeNull(d); null || err != nil {
		return err
	}
	return readMembers(d, func(name string, d *jsontext.Decoder) error {
		if name != "type" {
			return d.SkipValue()
		}
		v, err := readString(d)
		p.sandboxPolicy = v
		return err
	})
}

func decodeExec(params jsontext.Value) (execParams, error) {
	var p execParams
	if err := decodeParams(params, p.member); err != nil {
		return p, err
	}
	if err := p.validateShape(); err != nil {
		return p, err
	}
	return p, p.validateLimits()
}

func (p execParams) validateShape() error {
	switch {
	case len(p.command) == 0:
		return invalidParams("command must not be empty")
	case p.cwd != "" && !filepath.IsAbs(p.cwd):
		return invalidParams("cwd must be absolute")
	}
	if err := p.validateStreams(); err != nil {
		return err
	}
	return p.validateExclusive()
}

func (p execParams) validateStreams() error {
	switch {
	case p.tty:
		return invalidParams("tty is not supported")
	case p.streamStdin:
		return invalidParams("streamStdin is not supported")
	case p.size:
		return invalidParams("size needs tty")
	case p.streamStdoutStderr && p.processID == "":
		return invalidParams("streamStdoutStderr needs a processId")
	}
	return nil
}

func (p execParams) validateExclusive() error {
	switch {
	case p.hasTimeout && p.disableTimeout:
		return invalidParams("timeoutMs and disableTimeout exclude each other")
	case p.hasOutputCap && p.disableOutputCap:
		return invalidParams("outputBytesCap and disableOutputCap exclude each other")
	case p.sandboxPolicy == "externalSandbox":
		return invalidParams("unsupported sandboxPolicy externalSandbox")
	}
	return nil
}

func (p execParams) validateLimits() error {
	if p.timeoutMS < 0 {
		return invalidParams("timeoutMs must not be negative")
	}
	if p.hasOutputCap && (p.outputCap < 1 || p.outputCap > execMaxOutputCap) {
		return invalidParams(fmt.Sprintf("outputBytesCap must be from 1 to %d", execMaxOutputCap))
	}
	return nil
}

func (p execParams) spec(cfg config.Config) agent.ExecSpec {
	return agent.ExecSpec{
		Argv: p.command, Dir: cmp.Or(p.cwd, cfg.Cwd), Env: p.env, Sandbox: cfg.Sandbox,
		Timeout: p.timeout(), OutputCap: p.outputLimit(),
	}
}

func (p execParams) timeout() time.Duration {
	if p.disableTimeout {
		return 0
	}
	return time.Duration(cmp.Or(p.timeoutMS, execDefaultTimeoutMS)) * time.Millisecond
}

func (p execParams) outputLimit() int64 {
	if p.disableOutputCap {
		return 0
	}
	return int64(cmp.Or(p.outputCap, execDefaultOutputCap))
}

func (s *server) commandExec(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeExec(params)
	if err != nil {
		return nil, err
	}
	procCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	id, err := newExecID()
	if err != nil {
		return nil, internalError(err)
	}
	key := cmp.Or(p.processID, id)
	if err = s.startProcess(key, cancel); err != nil {
		return nil, err
	}
	defer s.finishProcess(key)
	spec := p.spec(s.host.Config())
	if p.streamStdoutStderr {
		spec.OnOutput = func(data []byte) {
			s.send("command/exec/outputDelta", commandOutputDelta{processID: p.processID, data: data})
		}
	}
	result, err := s.host.Exec(procCtx, id, spec)
	if err != nil {
		return nil, internalError(err)
	}
	if p.streamStdoutStderr {
		if result.CapReached {
			s.send("command/exec/outputDelta", commandOutputDelta{processID: p.processID, capReached: true})
		}
		return encode(CommandExecResponse{ExitCode: result.ExitCode})
	}
	return encode(CommandExecResponse{ExitCode: result.ExitCode, Stdout: strings.ToValidUTF8(string(result.Output), "\uFFFD")})
}

func (s *server) commandExecTerminate(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	var id string
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		if name != "processId" {
			return d.SkipValue()
		}
		v, err := readString(d)
		id = v
		return err
	})
	if err != nil {
		return nil, err
	}
	s.mu.Lock()
	cancel, running := s.procs[id]
	s.mu.Unlock()
	if !running {
		return nil, invalidParams("unknown process " + id)
	}
	cancel()
	return emptyObject(), nil
}

func (s *server) startProcess(id string, cancel context.CancelFunc) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.procs == nil {
		s.procs = map[string]context.CancelFunc{}
	}
	if _, running := s.procs[id]; running {
		return invalidParams("process " + id + " is running")
	}
	if len(s.procs) >= execMaxProcesses {
		return invalidParams("too many running commands")
	}
	s.procs[id] = cancel
	return nil
}

func (s *server) finishProcess(id string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.procs, id)
}

func newExecID() (string, error) {
	var raw [16]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("rpc: exec id: %w", err)
	}
	return "exec-" + hex.EncodeToString(raw[:]), nil
}
