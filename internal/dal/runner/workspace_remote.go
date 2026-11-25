package runner

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"strings"
	"time"
)

const (
	workspaceMethod         = "workspace/call"
	workspaceOpenMethod     = "workspace/open"
	workspaceCloseMethod    = "workspace/close"
	workspaceCheckMethod    = "workspace/check"
	rpcStaleRevision        = -32008
	rpcWorkspaceChanged     = -32012
	rpcWorkspaceUnsupported = -32013
)

var workspaceCodes = []struct {
	code int
	err  error
}{
	{-32003, ErrWorkspaceEscape},
	{-32004, ErrWorkspaceKind},
	{-32005, ErrWorkspaceExists},
	{-32006, ErrWorkspaceRoot},
	{-32007, ErrWorkspaceLimit},
	{-32009, ErrWorkspaceOffset},
	{-32010, ErrWorkspaceNotEmpty},
	{-32011, ErrWorkspaceRequest},
	{rpcWorkspaceChanged, ErrWorkspaceChanged},
	{rpcWorkspaceUnsupported, ErrWorkspaceUnsupported},
}

type workspaceRemoteError struct {
	kind error
	msg  string
}

func (e workspaceRemoteError) Error() string { return e.msg }

func (e workspaceRemoteError) Unwrap() error { return e.kind }

var _ Workspace = (*Remote)(nil)

var _ WorkspaceOpener = (*Remote)(nil)

var _ WorkspaceHandle = (*remoteWorkspaceHandle)(nil)

func (r *Remote) Workspace(ctx context.Context, req WorkspaceRequest) (WorkspaceResult, error) {
	params, err := json.Marshal(req)
	if err != nil {
		return WorkspaceResult{}, fmt.Errorf("runner: encode workspace request: %w", err)
	}
	raw, err := r.call(ctx, workspaceMethod, params)
	if err != nil {
		return WorkspaceResult{}, err
	}
	var result WorkspaceResult
	if err := json.Unmarshal(raw, &result); err != nil {
		return WorkspaceResult{}, fmt.Errorf("runner: decode workspace result: %w", err)
	}
	return result, nil
}

type workspaceOpenParams struct {
	Root string `json:"root"`
}

type workspaceOpened struct {
	Token string `json:"token"`
	Root  string `json:"root"`
}

type workspaceTokenParams struct {
	Token string `json:"token"`
}

type remoteWorkspaceHandle struct {
	remote  *Remote
	token   string
	display string
}

func (r *Remote) OpenWorkspace(ctx context.Context, cwd string) (WorkspaceHandle, error) {
	params, err := json.Marshal(workspaceOpenParams{Root: cwd})
	if err != nil {
		return nil, fmt.Errorf("runner: encode workspace open: %w", err)
	}
	raw, err := r.call(ctx, workspaceOpenMethod, params)
	if err != nil {
		if strings.Contains(err.Error(), "method not found") {
			return nil, ErrWorkspaceUnsupported
		}
		return nil, err
	}
	var opened workspaceOpened
	if err := json.Unmarshal(raw, &opened); err != nil {
		return nil, fmt.Errorf("runner: decode workspace open: %w", err)
	}
	if opened.Token == "" {
		return nil, ErrWorkspaceUnsupported
	}
	return &remoteWorkspaceHandle{remote: r, token: opened.Token, display: opened.Root}, nil
}

func (h *remoteWorkspaceHandle) Workspace(ctx context.Context, req WorkspaceRequest) (WorkspaceResult, error) {
	req.Token = h.token
	req.Root = ""
	params, err := json.Marshal(req)
	if err != nil {
		return WorkspaceResult{}, fmt.Errorf("runner: encode workspace request: %w", err)
	}
	raw, err := h.remote.call(ctx, workspaceMethod, params)
	if err != nil {
		return WorkspaceResult{}, err
	}
	var result WorkspaceResult
	if err := json.Unmarshal(raw, &result); err != nil {
		return WorkspaceResult{}, fmt.Errorf("runner: decode workspace result: %w", err)
	}
	return result, nil
}

func (h *remoteWorkspaceHandle) DisplayRoot() string {
	return h.display
}

func (h *remoteWorkspaceHandle) VerifyExec(ctx context.Context) error {
	params, err := json.Marshal(workspaceTokenParams{Token: h.token})
	if err != nil {
		return ErrWorkspaceChanged
	}
	if _, err := h.remote.call(ctx, workspaceCheckMethod, params); err != nil {
		return ErrWorkspaceChanged
	}
	return nil
}

func (h *remoteWorkspaceHandle) Close() error {
	params, err := json.Marshal(workspaceTokenParams{Token: h.token})
	if err != nil {
		return nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_, _ = h.remote.call(ctx, workspaceCloseMethod, params)
	return nil
}

func rpcWorkspace(ctx context.Context, local *Local, params jsontext.Value) (jsontext.Value, error) {
	var req WorkspaceRequest
	if err := json.Unmarshal(params, &req); err != nil {
		return nil, errInvalidParams
	}
	if req.Token != "" {
		table, ok := pinTableFromContext(ctx)
		if !ok {
			return nil, workspaceStatus(ErrWorkspaceChanged)
		}
		result, err := callBoxPin(ctx, table, req.Token, req)
		if err != nil {
			return nil, workspaceStatus(err)
		}
		return json.Marshal(result)
	}
	result, err := local.Workspace(ctx, req)
	if err != nil {
		return nil, workspaceStatus(err)
	}
	return json.Marshal(result)
}

func rpcWorkspaceOpen(ctx context.Context, _ *Local, params jsontext.Value) (jsontext.Value, error) {
	var req workspaceOpenParams
	if err := json.Unmarshal(params, &req); err != nil || req.Root == "" {
		return nil, errInvalidParams
	}
	table, ok := pinTableFromContext(ctx)
	if !ok {
		return nil, workspaceStatus(ErrWorkspaceUnsupported)
	}
	token, display, err := openBoxPin(ctx, table, req.Root)
	if err != nil {
		return nil, workspaceStatus(err)
	}
	return json.Marshal(workspaceOpened{Token: token, Root: display})
}

func rpcWorkspaceClose(ctx context.Context, _ *Local, params jsontext.Value) (jsontext.Value, error) {
	var req workspaceTokenParams
	if err := json.Unmarshal(params, &req); err != nil || req.Token == "" {
		return nil, errInvalidParams
	}
	if table, ok := pinTableFromContext(ctx); ok {
		closeBoxPin(table, req.Token)
	}
	return json.Marshal(emptyObject())
}

func rpcWorkspaceCheck(ctx context.Context, _ *Local, params jsontext.Value) (jsontext.Value, error) {
	var req workspaceTokenParams
	if err := json.Unmarshal(params, &req); err != nil || req.Token == "" {
		return nil, errInvalidParams
	}
	table, ok := pinTableFromContext(ctx)
	if !ok {
		return nil, workspaceStatus(ErrWorkspaceChanged)
	}
	if err := checkBoxPin(ctx, table, req.Token); err != nil {
		return nil, workspaceStatus(err)
	}
	return json.Marshal(emptyObject())
}

func workspaceStatus(err error) error {
	if stale, ok := errors.AsType[*StaleRevisionError](err); ok {
		return rpcStatusError{code: rpcStaleRevision, msg: stale.Current}
	}
	for _, known := range workspaceCodes {
		if errors.Is(err, known.err) {
			return rpcStatusError{code: known.code, msg: err.Error()}
		}
	}
	return err
}

func workspaceClientError(msg rpcMessage) error {
	if msg.errCode == rpcStaleRevision {
		return &StaleRevisionError{Current: msg.errMsg}
	}
	for _, known := range workspaceCodes {
		if msg.errCode == known.code {
			return workspaceRemoteError{kind: known.err, msg: msg.errMsg}
		}
	}
	return nil
}
