package runner

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"os"
	"sync"
)

type localWorkspaceHandle struct {
	root    *os.Root
	path    string
	display string
}

var _ WorkspaceHandle = (*localWorkspaceHandle)(nil)

var _ WorkspaceOpener = (*Local)(nil)

var _ Workspace = (*localWorkspaceHandle)(nil)

func (l *Local) OpenWorkspace(ctx context.Context, cwd string) (WorkspaceHandle, error) {
	if err := ctx.Err(); err != nil {
		return nil, fmt.Errorf("runner: open workspace: %w", err)
	}
	root, err := os.OpenRoot(cwd)
	if err != nil {
		return nil, fmt.Errorf("runner: open workspace root: %w", err)
	}
	info, err := root.Lstat(".")
	if err != nil {
		_ = root.Close()
		return nil, fmt.Errorf("runner: open workspace root: %w", err)
	}
	if !info.IsDir() {
		_ = root.Close()
		return nil, fmt.Errorf("runner: workspace root is not a directory")
	}
	return &localWorkspaceHandle{root: root, path: cwd, display: root.Name()}, nil
}

func (h *localWorkspaceHandle) Workspace(ctx context.Context, req WorkspaceRequest) (WorkspaceResult, error) {
	if err := ctx.Err(); err != nil {
		return WorkspaceResult{}, fmt.Errorf("runner: workspace: %w", err)
	}
	op, ok := workspaceOps[req.Op]
	if !ok {
		return WorkspaceResult{}, ErrWorkspaceRequest
	}
	result, err := op(h.root, req)
	result.Root = h.display
	return result, workspaceError(err)
}

func (h *localWorkspaceHandle) DisplayRoot() string {
	return h.display
}

func (h *localWorkspaceHandle) VerifyExec(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return ErrWorkspaceChanged
	}
	pinned, err := h.root.Lstat(".")
	if err != nil {
		return ErrWorkspaceChanged
	}
	live, err := os.Stat(h.path)
	if err != nil {
		return ErrWorkspaceChanged
	}
	if !os.SameFile(pinned, live) {
		return ErrWorkspaceChanged
	}
	return nil
}

func (h *localWorkspaceHandle) Close() error {
	return h.root.Close()
}

type pinTable struct {
	mu    sync.Mutex
	roots map[string]*localWorkspaceHandle
}

func newPinTable() *pinTable {
	return &pinTable{roots: map[string]*localWorkspaceHandle{}}
}

func (p *pinTable) store(handle *localWorkspaceHandle) (string, error) {
	var raw [16]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("runner: workspace token: %w", err)
	}
	token := hex.EncodeToString(raw[:])
	p.mu.Lock()
	p.roots[token] = handle
	p.mu.Unlock()
	return token, nil
}

func (p *pinTable) load(token string) (*localWorkspaceHandle, bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	handle, ok := p.roots[token]
	return handle, ok
}

func (p *pinTable) remove(token string) (*localWorkspaceHandle, bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	handle, ok := p.roots[token]
	if ok {
		delete(p.roots, token)
	}
	return handle, ok
}

func (p *pinTable) closeAll() {
	p.mu.Lock()
	handles := make([]*localWorkspaceHandle, 0, len(p.roots))
	for token, handle := range p.roots {
		handles = append(handles, handle)
		delete(p.roots, token)
	}
	p.mu.Unlock()
	for _, handle := range handles {
		_ = handle.Close()
	}
}

type pinTableKey struct{}

func contextWithPinTable(ctx context.Context) (context.Context, *pinTable) {
	table := newPinTable()
	return context.WithValue(ctx, pinTableKey{}, table), table
}

func pinTableFromContext(ctx context.Context) (*pinTable, bool) {
	table, ok := ctx.Value(pinTableKey{}).(*pinTable)
	return table, ok && table != nil
}

func openBoxPin(ctx context.Context, table *pinTable, cwd string) (string, string, error) {
	if err := ctx.Err(); err != nil {
		return "", "", fmt.Errorf("runner: open workspace: %w", err)
	}
	root, err := os.OpenRoot(cwd)
	if err != nil {
		return "", "", fmt.Errorf("runner: open workspace root: %w", err)
	}
	info, err := root.Lstat(".")
	if err != nil {
		_ = root.Close()
		return "", "", fmt.Errorf("runner: open workspace root: %w", err)
	}
	if !info.IsDir() {
		_ = root.Close()
		return "", "", fmt.Errorf("runner: workspace root is not a directory")
	}
	handle := &localWorkspaceHandle{root: root, path: cwd, display: root.Name()}
	token, err := table.store(handle)
	if err != nil {
		_ = root.Close()
		return "", "", err
	}
	return token, handle.display, nil
}

func callBoxPin(ctx context.Context, table *pinTable, token string, req WorkspaceRequest) (WorkspaceResult, error) {
	if err := ctx.Err(); err != nil {
		return WorkspaceResult{}, fmt.Errorf("runner: workspace: %w", err)
	}
	handle, ok := table.load(token)
	if !ok {
		return WorkspaceResult{}, fmt.Errorf("runner: workspace pin missing: %w", ErrWorkspaceChanged)
	}
	op, ok := workspaceOps[req.Op]
	if !ok {
		return WorkspaceResult{}, ErrWorkspaceRequest
	}
	result, err := op(handle.root, req)
	result.Root = handle.display
	return result, workspaceError(err)
}

func checkBoxPin(ctx context.Context, table *pinTable, token string) error {
	handle, ok := table.load(token)
	if !ok {
		return ErrWorkspaceChanged
	}
	return handle.VerifyExec(ctx)
}

func closeBoxPin(table *pinTable, token string) {
	handle, ok := table.remove(token)
	if ok {
		_ = handle.Close()
	}
}
