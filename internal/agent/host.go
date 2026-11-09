package agent

import (
	"bytes"
	"cmp"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"slices"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/keywords"
	"github.com/gosuda/dalgona/internal/model"
)

type Host struct {
	cfg      HostConfig
	journal  *journal.Store
	log      *slog.Logger
	clock    func() time.Time
	keywords []keywords.Keyword
	roots    []string

	mu       sync.Mutex
	threads  map[string]*Thread
	sessions map[string]ThreadInfo
	logged   map[string]bool
	models   map[string][]model.Model
}

func NewHost(cfg HostConfig) (*Host, error) {
	if cfg.Journal == nil || cfg.Providers == nil || cfg.Tools == nil || cfg.Runner == nil {
		return nil, errors.New("agent: host needs a journal, providers, a tool set, and a runner factory")
	}
	cfg.Product = cmp.Or(cfg.Product, cfg.Config.Product, config.ProductDalgona)
	h := &Host{
		cfg: cfg, journal: cfg.Journal, log: cmp.Or(cfg.Logger, slog.Default()), clock: cfg.Clock,
		threads: make(map[string]*Thread), sessions: make(map[string]ThreadInfo),
		logged: make(map[string]bool), models: make(map[string][]model.Model),
	}
	if h.clock == nil {
		h.clock = time.Now
	}
	table, err := customKeywords(cfg.Config)
	if err != nil {
		return nil, err
	}
	h.keywords = keywords.Merge(keywords.Builtins(string(cfg.Product)), table)
	h.roots = writableRoots(cfg.Config)
	if err := h.scanSessions(); err != nil {
		return nil, err
	}
	return h, nil
}

func customKeywords(cfg config.Config) ([]keywords.Keyword, error) {
	table := make([]keywords.Keyword, 0, len(cfg.Keywords))
	for word, spec := range cfg.Keywords {
		notice := spec.Text
		if spec.Notice != "" {
			data, err := os.ReadFile(filepath.Join(cfg.Home, spec.Notice))
			if err != nil {
				return nil, fmt.Errorf("agent: keyword %q notice: %w", word, err)
			}
			notice = string(data)
		}
		table = append(table, keywords.Keyword{
			Word: word, Hue: spec.Hue, Requires: spec.Requires, Effort: spec.Effort, Notice: notice, Enabled: spec.Enabled,
		})
	}
	slices.SortFunc(table, func(a, b keywords.Keyword) int { return cmp.Compare(a.Word, b.Word) })
	return table, nil
}

func writableRoots(cfg config.Config) []string {
	roots := []string{os.TempDir()}
	if cache, err := os.UserCacheDir(); err == nil {
		roots = append(roots, cache)
	}
	roots = append(roots, cfg.SandboxWritableRoots...)
	resolved := make([]string, 0, len(roots))
	for _, root := range roots {
		if root == "" {
			continue
		}
		if real, err := filepath.EvalSymlinks(root); err == nil {
			root = real
		}
		resolved = append(resolved, filepath.Clean(root))
	}
	return resolved
}

func (h *Host) scanSessions() error {
	heads, err := h.journal.Sessions()
	if err != nil {
		return fmt.Errorf("agent: list sessions: %w", err)
	}
	for _, head := range heads {
		info, err := h.sessionInfo(head)
		if err != nil {
			h.log.Warn("agent: skip session", "session", head.ID, "error", err)
			continue
		}
		h.sessions[head.ID] = info
	}
	return nil
}

func (h *Host) sessionInfo(head journal.SessionHead) (ThreadInfo, error) {
	info := ThreadInfo{ID: head.ID, Head: head.HeadSeq, Damaged: head.Status == journal.SessionDamaged}
	err := h.journal.Replay(head.ID, func(seq uint64, raw []byte) error {
		if !info.wants(raw) {
			return nil
		}
		decoded, err := event.Decode(raw)
		if err != nil {
			return &journal.DamageError{Session: head.ID, Seq: seq}
		}
		h.applyInfo(&info, seq, decoded)
		return nil
	})
	if err != nil {
		if _, damaged := errors.AsType[*journal.DamageError](err); !damaged {
			return ThreadInfo{}, err
		}
		info.Damaged = true
		return info, nil
	}
	if info.Cwd == "" && !info.Damaged {
		return ThreadInfo{}, fmt.Errorf("agent: session %s has no session.created", head.ID)
	}
	if stat, err := os.Stat(filepath.Join(h.cfg.Config.Home, "sessions", head.ID, "journal.jsonl")); err == nil {
		info.Updated = stat.ModTime().UTC()
	}
	return info, nil
}

func metadataLine(raw []byte) bool {
	return bytes.HasPrefix(raw, []byte(`{"type":"session.created"`)) || bytes.HasPrefix(raw, []byte(`{"type":"thread.`))
}

func promptLine(raw []byte) bool { return bytes.HasPrefix(raw, []byte(`{"type":"user.message"`)) }

func (info *ThreadInfo) wants(raw []byte) bool {
	if metadataLine(raw) || bytes.HasPrefix(raw, []byte(`{"type":"branch"`)) {
		return true
	}
	return promptLine(raw) && (info.Preview == "" || bytes.Contains(raw, []byte(`"model":`)))
}

type modelTrail struct {
	created string
	marks   []event.Seqd
}

func (m *modelTrail) note(seq uint64, e event.Event) bool {
	switch value := e.(type) {
	case event.Branch:
		m.markParent(value.ParentSeq)
		m.marks = append(m.marks, event.Seqd{Seq: seq, Event: value})
		return true
	case event.UserMessage:
		if value.Model == "" {
			return false
		}
		m.marks = append(m.marks, event.Seqd{Seq: seq, Event: event.UserMessage{Model: value.Model}})
		return true
	}
	return false
}

func (m *modelTrail) markParent(parent uint64) {
	i, found := slices.BinarySearchFunc(m.marks, parent, func(item event.Seqd, seq uint64) int { return cmp.Compare(item.Seq, seq) })
	if parent != 0 && !found {
		m.marks = slices.Insert(m.marks, i, event.Seqd{Seq: parent})
	}
}

func (m *modelTrail) model() string {
	out := m.created
	for _, item := range event.ActivePath(m.marks) {
		if message, ok := item.Event.(event.UserMessage); ok {
			out = message.Model
		}
	}
	return out
}

func (h *Host) applyInfo(info *ThreadInfo, seq uint64, e event.Event) {
	if info.models.note(seq, e) {
		info.Model = info.models.model()
	}
	switch value := e.(type) {
	case event.SessionCreated:
		info.Parent, info.ForkOf, info.Advisor = value.Parent, value.ForkOf, value.Advisor
		info.Cwd, info.Model, info.models.created, info.Created = value.Cwd, value.Model, value.Model, value.CreatedAt
	case event.ThreadNamed:
		info.Name = value.Name
	case event.ThreadArchived:
		info.Archived = value.Archived
	case event.UserMessage:
		h.applyPreview(info, value)
	}
}

func (h *Host) applyPreview(info *ThreadInfo, value event.UserMessage) {
	if info.Preview != "" || value.Kind != string(InputPrompt) {
		return
	}
	text, err := h.loadText(value.Text, value.TextBlob)
	if err != nil {
		h.log.Warn("agent: thread preview", "thread", info.ID, "error", err)
	}
	info.Preview = previewRunes(text, 80)
}

func (h *Host) Threads() []ThreadInfo {
	return slices.DeleteFunc(h.AllThreads(), func(info ThreadInfo) bool { return info.Archived })
}

func (h *Host) AllThreads() []ThreadInfo {
	h.mu.Lock()
	defer h.mu.Unlock()
	out := make([]ThreadInfo, 0, len(h.sessions))
	for _, info := range h.sessions {
		if info.Advisor == "" {
			out = append(out, info)
		}
	}
	slices.SortFunc(out, func(a, b ThreadInfo) int { return cmp.Compare(a.ID, b.ID) })
	return out
}

func (h *Host) AdvisorChild(parent, name string) (string, bool) {
	h.mu.Lock()
	defer h.mu.Unlock()
	for _, info := range h.sessions {
		if info.Parent == parent && info.Advisor == name {
			return info.ID, true
		}
	}
	return "", false
}

func (h *Host) recoverable() []string {
	h.mu.Lock()
	defer h.mu.Unlock()
	ids := make([]string, 0, len(h.sessions))
	for id, info := range h.sessions {
		if info.Advisor == "" && !info.Damaged {
			ids = append(ids, id)
		}
	}
	slices.Sort(ids)
	return ids
}

func (h *Host) Recover(ctx context.Context) error {
	var errs []error
	for _, id := range h.recoverable() {
		open, err := h.turnOpen(id)
		if err != nil {
			errs = append(errs, err)
			continue
		}
		if !open {
			continue
		}
		if _, err := h.Resume(ctx, id); err != nil {
			errs = append(errs, fmt.Errorf("agent: recover %s: %w", id, err))
		}
	}
	return errors.Join(errs...)
}

func (h *Host) turnOpen(id string) (bool, error) {
	loaded, err := h.load(id)
	if err != nil {
		return false, err
	}
	return loaded.state.TurnID != "", nil
}

func (h *Host) Start(ctx context.Context, spec ThreadSpec) (*Thread, error) {
	return h.startSession(ctx, spec, h.newThreadID())
}

func (h *Host) startSession(ctx context.Context, spec ThreadSpec, id string) (*Thread, error) {
	created, err := h.sessionCreated(spec, id)
	if err != nil {
		return nil, err
	}
	thread, err := h.openThread(ctx, loadedSession{created: created}, spec.Overrides)
	if err != nil {
		return nil, err
	}
	if _, err = thread.appendEvents(ctx, created); err != nil {
		return nil, errors.Join(err, thread.Close())
	}
	return thread, nil
}

func (h *Host) sessionCreated(spec ThreadSpec, id string) (event.SessionCreated, error) {
	cfg := h.cfg.Config
	created := event.SessionCreated{
		ID: id, Parent: spec.Parent, Advisor: spec.Advisor, CreatedAt: h.clock().UTC(),
		Cwd:      cmp.Or(spec.Cwd, cfg.Cwd),
		Model:    cmp.Or(spec.Model, cfg.Model),
		Approval: string(cmp.Or(spec.Approval, cfg.Approval, config.ApprovalAuto)),
		Sandbox:  string(cmp.Or(spec.Sandbox, cfg.Sandbox, config.SandboxWorkspaceWrite)),
		Effort:   string(spec.Effort),
		Runner:   cmp.Or(spec.Runner, cfg.Runner, "local"),
	}
	if created.Model == "" {
		return created, errors.New("agent: no model is configured; pick one with /model")
	}
	if created.Cwd == "" {
		return created, errors.New("agent: thread needs a working directory")
	}
	if spec.Advisor != "" && spec.Tools == nil {
		return created, ErrNeedsOverrides
	}
	if spec.System != "" {
		return created, nil
	}
	instructions := instructionsText(cfg.Home, created.Cwd)
	if instructions == "" {
		return created, nil
	}
	sha, err := h.journal.PutBlob([]byte(instructions))
	if err != nil {
		return created, fmt.Errorf("agent: store instructions: %w", err)
	}
	created.InstructionsBlob = &event.BlobRef{Blob: sha, Bytes: int64(len(instructions))}
	return created, nil
}

func (h *Host) Resume(ctx context.Context, id string) (*Thread, error) {
	return h.ResumeWith(ctx, id, Overrides{})
}

func (h *Host) ResumeWith(ctx context.Context, id string, overrides Overrides) (*Thread, error) {
	h.mu.Lock()
	if thread, ok := h.threads[id]; ok {
		h.mu.Unlock()
		return thread, nil
	}
	h.mu.Unlock()
	release, err := h.lockSession(id)
	if err != nil {
		return nil, err
	}
	loaded, err := h.load(id)
	if err != nil {
		return nil, errors.Join(err, release())
	}
	if loaded.created.Advisor != "" && overrides.Tools == nil {
		return nil, errors.Join(ErrNeedsOverrides, release())
	}
	return h.openLocked(ctx, loaded, overrides, release)
}

func (h *Host) Close() error {
	h.mu.Lock()
	threads := make([]*Thread, 0, len(h.threads))
	for _, thread := range h.threads {
		threads = append(threads, thread)
	}
	h.mu.Unlock()
	var errs []error
	for _, thread := range threads {
		errs = append(errs, thread.Close())
	}
	return errors.Join(errs...)
}

func (h *Host) logOnce(key, message string, args ...any) {
	h.mu.Lock()
	seen := h.logged[key]
	h.logged[key] = true
	h.mu.Unlock()
	if !seen {
		h.log.Info(message, args...)
	}
}

func (h *Host) forget(id string) {
	h.mu.Lock()
	delete(h.threads, id)
	h.mu.Unlock()
}

func (h *Host) loadedThread(id string) (*Thread, bool) {
	h.mu.Lock()
	defer h.mu.Unlock()
	thread, ok := h.threads[id]
	return thread, ok && id != ""
}

func (h *Host) loadedChildren(parent string) []*Thread {
	h.mu.Lock()
	defer h.mu.Unlock()
	var children []*Thread
	for _, thread := range h.threads {
		if thread.created.Parent == parent && thread.created.Advisor == "" {
			children = append(children, thread)
		}
	}
	return children
}

func (h *Host) noteAppend(id string, head uint64, events []event.Event) {
	h.mu.Lock()
	info, ok := h.sessions[id]
	h.mu.Unlock()
	if !ok {
		info = ThreadInfo{ID: id}
	}
	info.Head, info.Updated = head, h.clock().UTC()
	first := head - uint64(len(events)) + 1
	for i, item := range events {
		h.applyInfo(&info, first+uint64(i), item)
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	h.sessions[id] = info
}
