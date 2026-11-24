package state

import (
	"encoding/base64"
	"errors"
	"fmt"
	"maps"
	"slices"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	methodWorkspaceOpen   = "dalgona/workspace/open"
	methodWorkspaceClose  = "dalgona/workspace/close"
	methodWorkspaceSearch = "dalgona/workspace/search"
	methodWorkspaceExec   = "dalgona/workspace/exec"
	rightRead             = "read"
	rightTerminal         = "terminal"
	decisionDeadline      = 6 * time.Minute
	execDeadline          = 125 * time.Second
	searchLimit           = 20
	shellOutputLimit      = 64 << 10
	shellRunLimit         = 32
)

type (
	SearchPhase uint8
	ShellPhase  uint8
)

const (
	SearchAuthorizing SearchPhase = iota
	SearchDeclined
	SearchNoDirectory
	SearchSearching
	SearchDone
	SearchFailed
)

const (
	ShellAuthorizing ShellPhase = iota
	ShellDeclined
	ShellRunning
	ShellExited
	ShellTimedOut
	ShellCancelled
	ShellFailed
	ShellUnknown
)

var errWorkspaceRefused = errors.New("workspace access was declined")

type (
	SearchFiles struct {
		Key    DraftKey
		Query  string
		Ticket uint64
	}
	RunShell struct {
		Key     DraftKey
		Command string
		Ticket  uint64
	}
	StopShell struct {
		Key    DraftKey
		Ticket uint64
	}
	FileSearch struct {
		Ticket     uint64
		Query      string
		Root       string
		Phase      SearchPhase
		Paths      []string
		Incomplete bool
		Err        error
	}
	ShellRun struct {
		Ticket      uint64
		N           int
		Command     string
		Root        string
		Phase       ShellPhase
		ExitCode    *int
		Output      string
		Truncated   bool
		OperationID string
		Err         error
	}
	workspaceKey struct {
		thread ThreadKey
		right  string
	}
	workspace struct {
		link   client.Link
		id     string
		root   string
		ticket uint64
	}
	searchState struct {
		view     FileSearch
		call     uint64
		reopened bool
	}
	shellState struct {
		view ShellRun
		call uint64
	}
	workspaceOpenCall struct {
		key  workspaceKey
		link client.Link
	}
	searchCall struct {
		key    DraftKey
		ticket uint64
	}
	shellCall struct {
		key    DraftKey
		ticket uint64
	}
	workspaceOpenParams struct {
		ThreadID    string   `json:"threadId"`
		Root        string   `json:"root"`
		Rights      []string `json:"rights"`
		OperationID string   `json:"operationId"`
	}
	workspaceOpenNote struct {
		WorkspaceID string `json:"workspaceId"`
		Root        string `json:"root"`
	}
	workspaceParams struct {
		WorkspaceID string `json:"workspaceId"`
	}
	searchParams struct {
		WorkspaceID string `json:"workspaceId"`
		Query       string `json:"query"`
		Limit       int    `json:"limit"`
	}
	searchNote struct {
		Entries []struct {
			Path string `json:"path"`
		} `json:"entries"`
		Incomplete bool `json:"incomplete"`
	}
	execParams struct {
		WorkspaceID string `json:"workspaceId"`
		Text        string `json:"text"`
		OperationID string `json:"operationId"`
	}
	execNote struct {
		ExitCode        *int   `json:"exitCode"`
		OutputBase64    string `json:"outputBase64"`
		OutputTruncated bool   `json:"outputTruncated"`
		Termination     string `json:"termination"`
	}
)

var terminations = map[string]ShellPhase{"exited": ShellExited, "timedOut": ShellTimedOut, "cancelled": ShellCancelled}

func (i SearchFiles) apply(m *machine) {
	m.searches[i.Key] = &searchState{view: FileSearch{Ticket: i.Ticket, Query: i.Query, Phase: SearchAuthorizing}}
	m.acquire(workspaceKey{thread: i.Key.thread(), right: rightRead})
}

func (i RunShell) apply(m *machine) {
	runs := m.shells[i.Key]
	run := &shellState{view: ShellRun{Ticket: i.Ticket, N: len(runs) + 1, Command: i.Command, Phase: ShellAuthorizing}}
	if len(runs) > 0 {
		run.view.N = runs[len(runs)-1].view.N + 1
	}
	m.shells[i.Key] = append(runs[max(0, len(runs)-shellRunLimit+1):], run)
	m.acquire(workspaceKey{thread: i.Key.thread(), right: rightTerminal})
}

func (i StopShell) apply(m *machine) {
	if run := m.shellRun(i.Key, i.Ticket); run != nil && run.view.Phase == ShellRunning {
		m.emit(cancelEffect{ticket: run.call})
	}
}

func (m *machine) shellRun(key DraftKey, ticket uint64) *shellState {
	at := slices.IndexFunc(m.shells[key], func(s *shellState) bool { return s.view.Ticket == ticket })
	if at < 0 {
		return nil
	}
	return m.shells[key][at]
}

func (m *machine) acquire(key workspaceKey) {
	ep := m.endpoint(key.thread.Endpoint)
	m.use(ep)
	if !ep.live {
		return
	}
	ws := m.workspaces[key]
	switch {
	case ws != nil && ws.link == ep.link && ws.id != "":
		m.workspaceReady(ep, key, ws)
	case ws != nil && ws.link == ep.link:
	default:
		ws = &workspace{link: ep.link}
		m.workspaces[key] = ws
		params := workspaceOpenParams{ThreadID: key.thread.Thread, Root: "cwd", Rights: []string{key.right}, OperationID: rpc.NewOperationID()}
		ws.ticket = m.call(ep, workspaceOpenCall{key: key, link: ep.link}, methodWorkspaceOpen, params)
	}
}

func (c workspaceOpenCall) resume(m *machine, r resultMsg) {
	ws := m.workspaces[c.key]
	ep := m.endpoints[c.link.Endpoint]
	if ws == nil || ws.ticket != r.ticket || !ep.live || ep.link != c.link {
		return
	}
	ws.ticket = 0
	note, err := decode[workspaceOpenNote](r.raw)
	if r.err != nil {
		err = r.err
	}
	if err != nil {
		delete(m.workspaces, c.key)
		m.workspaceFailed(c.key, err)
		return
	}
	ws.id, ws.root = note.WorkspaceID, note.Root
	m.workspaceReady(ep, c.key, ws)
}

func (m *machine) waiters(key workspaceKey) []DraftKey {
	var keys []DraftKey
	for draft := range m.searches {
		if key.right == rightRead && draft.thread() == key.thread {
			keys = append(keys, draft)
		}
	}
	for draft := range m.shells {
		if key.right == rightTerminal && draft.thread() == key.thread {
			keys = append(keys, draft)
		}
	}
	slices.SortFunc(keys, compareDraftKeys)
	return keys
}

func (m *machine) workspaceReady(ep *endpointState, key workspaceKey, ws *workspace) {
	for _, draft := range m.waiters(key) {
		if s := m.searches[draft]; key.right == rightRead && s.view.Phase == SearchAuthorizing {
			s.view.Phase, s.view.Root = SearchSearching, ws.root
			s.call = m.call(ep, searchCall{key: draft, ticket: s.view.Ticket}, methodWorkspaceSearch, searchParams{WorkspaceID: ws.id, Query: s.view.Query, Limit: searchLimit})
		}
		for _, run := range m.shells[draft] {
			if key.right == rightTerminal && run.view.Phase == ShellAuthorizing {
				m.dispatchShell(ep, draft, run, ws)
			}
		}
	}
}

func (m *machine) dispatchShell(ep *endpointState, key DraftKey, run *shellState, ws *workspace) {
	run.view.Phase, run.view.Root, run.view.OperationID = ShellRunning, ws.root, rpc.NewOperationID()
	link, params := ep.link, execParams{WorkspaceID: ws.id, Text: run.view.Command, OperationID: run.view.OperationID}
	run.call = m.spawn(ep, shellCall{key: key, ticket: run.view.Ticket}, func(ticket uint64) effect {
		return callEffect{ticket: ticket, link: link, method: methodWorkspaceExec, params: params, deadline: execDeadline}
	})
}

var (
	refusedKinds   = map[rpc.ErrorKind]bool{rpc.ErrorPermissionDenied: true, rpc.ErrorCancelled: true}
	directoryKinds = map[rpc.ErrorKind]bool{rpc.ErrorNotFound: true, rpc.ErrorIncompatible: true}
)

func (m *machine) workspaceFailed(key workspaceKey, err error) {
	search, shell := SearchFailed, ShellFailed
	data, _ := rpc.ErrorDataOf(err)
	if refusedKinds[data.Kind] {
		search, shell, err = SearchDeclined, ShellDeclined, fmt.Errorf("%w: %w", errWorkspaceRefused, err)
	}
	if directoryKinds[data.Kind] {
		search = SearchNoDirectory
	}
	for _, draft := range m.waiters(key) {
		if s := m.searches[draft]; key.right == rightRead && s.view.Phase == SearchAuthorizing {
			s.view.Phase, s.view.Err = search, err
		}
		for _, run := range m.shells[draft] {
			if key.right == rightTerminal && run.view.Phase == ShellAuthorizing {
				run.view.Phase, run.view.Err = shell, err
			}
		}
	}
}

func (c searchCall) resume(m *machine, r resultMsg) {
	s := m.searches[c.key]
	if s == nil || s.view.Ticket != c.ticket || s.call != r.ticket {
		return
	}
	s.call = 0
	if expired(r.err) && !s.reopened {
		s.reopened, s.view.Phase = true, SearchAuthorizing
		delete(m.workspaces, workspaceKey{thread: c.key.thread(), right: rightRead})
		m.acquire(workspaceKey{thread: c.key.thread(), right: rightRead})
		return
	}
	note, err := decode[searchNote](r.raw)
	if r.err != nil {
		err = r.err
	}
	if err != nil {
		s.view.Phase, s.view.Err = SearchFailed, err
		return
	}
	s.view.Phase, s.view.Incomplete, s.view.Paths = SearchDone, note.Incomplete, nil
	for _, entry := range note.Entries[:min(len(note.Entries), searchLimit)] {
		s.view.Paths = append(s.view.Paths, entry.Path)
	}
}

func (c shellCall) resume(m *machine, r resultMsg) {
	run := m.shellRun(c.key, c.ticket)
	if run == nil || run.call != r.ticket {
		return
	}
	run.call = 0
	switch outcome(run.view.OperationID, r.err) {
	case outcomeSucceeded:
		run.settle(r.raw)
	case outcomeFailed, outcomeUndispatched:
		run.view.Phase, run.view.Err = ShellFailed, r.err
	case outcomeRunning, outcomeUnknown:
		run.view.Phase, run.view.Err = ShellUnknown, r.err
	}
}

func (run *shellState) settle(raw []byte) {
	note, err := decode[execNote](raw)
	output, decodeErr := base64.StdEncoding.DecodeString(note.OutputBase64)
	phase, known := terminations[note.Termination]
	if err = errors.Join(err, decodeErr); err != nil || !known {
		run.view.Phase, run.view.Err = ShellFailed, cmpError(err, fmt.Errorf("the command ended with termination %q", note.Termination))
		return
	}
	run.view.Phase, run.view.ExitCode, run.view.Err = phase, note.ExitCode, nil
	run.view.Output, run.view.Truncated = string(output[:min(len(output), shellOutputLimit)]), note.OutputTruncated || len(output) > shellOutputLimit
}

func cmpError(err, fallback error) error {
	if err != nil {
		return err
	}
	return fallback
}

func (m *machine) checkShell(operationID string) bool {
	for key, runs := range m.shells {
		for _, run := range runs {
			if run.view.OperationID == operationID && run.view.Phase == ShellUnknown {
				m.readShellReceipt(key, run)
				return true
			}
		}
	}
	return false
}

func (m *machine) readShellReceipt(key DraftKey, run *shellState) {
	ep := m.endpoint(key.Endpoint)
	m.use(ep)
	if ep.live {
		run.call = m.call(ep, shellReceiptCall{key: key, ticket: run.view.Ticket}, methodOperationRead, operationParams{OperationID: run.view.OperationID})
	}
}

type shellReceiptCall struct {
	key    DraftKey
	ticket uint64
}

func (c shellReceiptCall) resume(m *machine, r resultMsg) {
	run := m.shellRun(c.key, c.ticket)
	if run == nil || run.call != r.ticket {
		return
	}
	run.call = 0
	if data, coded := rpc.ErrorDataOf(r.err); coded && data.Kind == rpc.ErrorNotFound {
		run.view.Phase, run.view.Err = ShellFailed, errNeverStarted
		return
	}
	record, err := decode[rpc.OperationRecord](r.raw)
	switch {
	case r.err != nil || err != nil:
		run.view.Err = cmpError(r.err, err)
	case record.State == "succeeded":
		run.settle(record.Result)
	case record.State == "failed":
		run.view.Phase, run.view.Err = ShellFailed, recordError(record)
	}
}

func (m *machine) releaseWorkspaces(ep *endpointState, thread ThreadKey) {
	for key, ws := range m.workspaces {
		if key.thread != thread {
			continue
		}
		delete(m.workspaces, key)
		if ep.live && ws.link == ep.link && ws.id != "" {
			m.call(ep, releaseCall{}, methodWorkspaceClose, workspaceParams{WorkspaceID: ws.id})
		}
	}
}

func (m *machine) expireWorkspaces(ep *endpointState) {
	for key := range m.workspaces {
		if key.thread.Endpoint == ep.id {
			delete(m.workspaces, key)
		}
	}
}

func (m *machine) resumeWorkspaces(ep *endpointState) {
	for _, draft := range slices.SortedFunc(maps.Keys(m.searches), compareDraftKeys) {
		if s := m.searches[draft]; draft.Endpoint == ep.id && s.view.Phase == SearchAuthorizing {
			m.acquire(workspaceKey{thread: draft.thread(), right: rightRead})
		}
	}
	for _, draft := range slices.SortedFunc(maps.Keys(m.shells), compareDraftKeys) {
		authorizing := slices.ContainsFunc(m.shells[draft], func(r *shellState) bool { return r.view.Phase == ShellAuthorizing })
		if draft.Endpoint == ep.id && authorizing {
			m.acquire(workspaceKey{thread: draft.thread(), right: rightTerminal})
		}
	}
}
