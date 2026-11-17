package report

import (
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
	"time"
)

const ghTokenMaxBytes = 4096

var ghTokenEnvKeys = map[string]bool{
	"GH_TOKEN": true, "GITHUB_TOKEN": true,
	"GH_ENTERPRISE_TOKEN": true, "GITHUB_ENTERPRISE_TOKEN": true,
}

type GHSource struct {
	lookPath func(string) (string, error)
	run      func(ctx context.Context, exe string) ([]byte, error)
	mu       sync.Mutex
	cached   string
}

func NewGHSource() *GHSource {
	return &GHSource{lookPath: exec.LookPath}
}

func (s *GHSource) BeginRun() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.cached = ""
}

func (s *GHSource) Credential(ctx context.Context) (Credential, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.cached != "" {
		return Credential{Access: s.cached}, nil
	}
	out, err := s.fetch(ctx)
	if err != nil {
		return Credential{}, err
	}
	token, err := parseGHToken(string(out))
	if err != nil {
		return Credential{}, err
	}
	s.cached = token
	return Credential{Access: token}, nil
}

func (s *GHSource) fetch(ctx context.Context) ([]byte, error) {
	if s.run != nil {
		exe, err := s.resolveExe()
		if err != nil {
			return nil, err
		}
		return s.run(ctx, exe)
	}
	exe, err := s.resolveExe()
	if err != nil {
		return nil, err
	}
	deadline, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	cmd := exec.CommandContext(deadline, exe, "auth", "token", "--hostname", "github.com", "--no-refresh")
	cmd.Stdin = nil
	cmd.Env = strippedGHEnv(os.Environ())
	var out bytes.Buffer
	cmd.Stdout = &cappedWriter{w: &out, cap: ghTokenMaxBytes + 1}
	cmd.Stderr = io.Discard
	if err := cmd.Run(); err != nil {
		return nil, errors.New("failure reporting gh credential unavailable")
	}
	return out.Bytes(), nil
}

func (s *GHSource) resolveExe() (string, error) {
	lookPath := s.lookPath
	if lookPath == nil {
		lookPath = exec.LookPath
	}
	exe, err := lookPath("gh")
	if err != nil {
		return "", errors.New("failure reporting gh executable not found")
	}
	return exe, nil
}

func strippedGHEnv(env []string) []string {
	kept := make([]string, 0, len(env))
	for _, entry := range env {
		key, _, _ := strings.Cut(entry, "=")
		if !ghTokenEnvKeys[key] {
			kept = append(kept, entry)
		}
	}
	return kept
}

func parseGHToken(raw string) (string, error) {
	token, found := strings.CutSuffix(raw, "\r\n")
	if !found {
		token, _ = strings.CutSuffix(raw, "\n")
	}
	if token == "" || len(token) > ghTokenMaxBytes {
		return "", errors.New("failure reporting gh credential unavailable")
	}
	for _, r := range token {
		if r < 0x20 || r == 0x7f {
			return "", errors.New("failure reporting gh credential unavailable")
		}
	}
	return token, nil
}

type cappedWriter struct {
	w   io.Writer
	n   int
	cap int
}

func (c *cappedWriter) Write(p []byte) (int, error) {
	c.n += len(p)
	if c.n > c.cap {
		return len(p), nil
	}
	return c.w.Write(p)
}
