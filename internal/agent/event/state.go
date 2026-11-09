package event

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"maps"
	"slices"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/agent/rules"
)

type Status string

const (
	StatusIdle             Status = "idle"
	StatusTurnRunning      Status = "turn_running"
	StatusAwaitingApproval Status = "awaiting_approval"
	StatusDamaged          Status = "damaged"
)

type ToolState struct {
	CallID   string
	Tool     string
	Class    string
	Args     jsontext.Value
	ArgsBlob *BlobRef
}

type ApprovalState struct {
	CallID      string
	Summary     string
	Class       string
	GrantPrefix string
}

type ScriptState struct {
	NextEffect uint64
	Seen       map[uint64]struct{}
}

type ChildState struct {
	CallID string
	Runner string
}

type State struct {
	Status          Status
	SessionID       string
	TurnID          string
	InFlight        map[string]ToolState
	PendingCalls    map[string]ToolCall
	Approvals       map[string]ApprovalState
	SessionAccepted map[string]struct{}
	DeclinedCalls   map[string]string
	Scripts         map[string]ScriptState
	Children        map[string]ChildState
	Runner          *RunnerBound
	LastCompaction  *Compaction
	Todos           []TodoItem
	TodoBlob        *BlobRef
	Grants          map[string]string
	Snapshots       map[string]Snapshot
	CompletedTurns  map[string]uint64
	Notes           map[string]uint64
	Name            string
	Archived        bool
}

func Fold(s State, seq uint64, e Event) (State, error) {
	next := s.clone()
	if next.Status == "" {
		next.Status = StatusIdle
	}
	if next.Status == StatusDamaged {
		return next, errors.New("event: fold: session is damaged")
	}
	if seq == 0 || e == nil {
		return next.damage(errors.New("event: fold needs a positive sequence and an event"))
	}
	if err := e.Validate(); err != nil {
		return next.damage(fmt.Errorf("event: fold validation: %w", err))
	}
	if err := foldEvent(&next, seq, e); err != nil {
		return next.damage(err)
	}
	next.Status = next.deriveStatus()
	return next, nil
}

func (s State) clone() State {
	s.InFlight = maps.Clone(s.InFlight)
	s.PendingCalls = maps.Clone(s.PendingCalls)
	s.Approvals = maps.Clone(s.Approvals)
	s.SessionAccepted = maps.Clone(s.SessionAccepted)
	s.DeclinedCalls = maps.Clone(s.DeclinedCalls)
	s.Scripts = maps.Clone(s.Scripts)
	for id, run := range s.Scripts {
		run.Seen = maps.Clone(run.Seen)
		s.Scripts[id] = run
	}
	s.Children = maps.Clone(s.Children)
	s.Grants = maps.Clone(s.Grants)
	s.Snapshots = maps.Clone(s.Snapshots)
	s.CompletedTurns = maps.Clone(s.CompletedTurns)
	s.Notes = maps.Clone(s.Notes)
	s.Todos = slices.Clone(s.Todos)
	s.TodoBlob = cloneBlob(s.TodoBlob)
	if s.Runner != nil {
		value := *s.Runner
		s.Runner = &value
	}
	if s.LastCompaction != nil {
		value := *s.LastCompaction
		s.LastCompaction = &value
	}
	return s
}

func cloneBlob(blob *BlobRef) *BlobRef {
	if blob == nil {
		return nil
	}
	value := *blob
	return &value
}

func (s State) damage(err error) (State, error) {
	s.Status = StatusDamaged
	return s, err
}

func (s State) deriveStatus() Status {
	if s.TurnID == "" {
		return StatusIdle
	}
	if len(s.Approvals) > 0 {
		return StatusAwaitingApproval
	}
	return StatusTurnRunning
}

func foldEvent(s *State, seq uint64, e Event) error {
	switch e.Type() {
	case TypeSessionCreated, TypeRunnerBound, TypeTurnStarted, TypeTurnCompleted, TypeUserMessage:
		return foldSessionEvent(s, seq, e)
	case TypeAssistantMessage, TypeToolStarted, TypeToolCompleted, TypeModelRetry, TypeApprovalRequested, TypeApprovalResolved:
		return foldToolEvent(s, seq, e)
	case TypeCompaction, TypeContextPruned, TypeChildSpawned, TypeBranch:
		return foldStateEvent(s, seq, e)
	case TypeScriptStarted, TypeScriptEffect, TypeScriptCompleted:
		return foldScriptEvent(s, e)
	case TypeAdvisorNote, TypeThreadNamed, TypeThreadArchived:
		return foldMetadata(s, seq, e)
	default:
		return fmt.Errorf("event: fold: unknown type %q", e.Type())
	}
}

func foldSessionEvent(s *State, seq uint64, e Event) error {
	switch value := e.(type) {
	case SessionCreated:
		if s.SessionID != "" || s.TurnID != "" {
			return errors.New("event: fold: duplicate session.created")
		}
		s.SessionID = value.ID
		return nil
	case RunnerBound:
		copy := value
		s.Runner = &copy
		return nil
	case TurnStarted:
		return startTurn(s, value.TurnID)
	case TurnCompleted:
		return completeTurn(s, seq, value)
	case UserMessage:
		return requireTurn(s, value.TurnID)
	default:
		return fmt.Errorf("event: fold: %T does not match %s", e, e.Type())
	}
}

func startTurn(s *State, turnID string) error {
	if s.TurnID != "" {
		return errors.New("event: fold: turn.started while a turn runs")
	}
	if _, exists := s.CompletedTurns[turnID]; exists {
		return fmt.Errorf("event: fold: duplicate turn %s", turnID)
	}
	s.TurnID = turnID
	return nil
}

func completeTurn(s *State, seq uint64, e TurnCompleted) error {
	if err := requireTurn(s, e.TurnID); err != nil {
		return err
	}
	if len(s.InFlight)+len(s.PendingCalls)+len(s.Approvals)+len(s.Scripts) > 0 {
		return errors.New("event: fold: turn.completed with unfinished work")
	}
	if s.CompletedTurns == nil {
		s.CompletedTurns = make(map[string]uint64)
	}
	s.CompletedTurns[e.TurnID] = seq
	s.TurnID = ""
	return nil
}

func requireTurn(s *State, turnID string) error {
	if s.TurnID == "" || s.TurnID != turnID {
		return fmt.Errorf("event: fold: event turn %s does not match open turn %s", turnID, s.TurnID)
	}
	return nil
}

func foldToolEvent(s *State, seq uint64, e Event) error {
	switch value := e.(type) {
	case AssistantMessage:
		return foldAssistantMessage(s, seq, value)
	case ToolStarted:
		return foldToolStarted(s, value)
	case ToolCompleted:
		return foldToolCompleted(s, value)
	case ModelRetry:
		return requireTurn(s, value.TurnID)
	case ApprovalRequested:
		return foldApprovalRequested(s, value)
	case ApprovalResolved:
		return foldApprovalResolved(s, value)
	default:
		return fmt.Errorf("event: fold: %T does not match %s", e, e.Type())
	}
}

func foldAssistantMessage(s *State, seq uint64, e AssistantMessage) error {
	if err := requireTurn(s, e.TurnID); err != nil {
		return err
	}
	if e.ContextSeq >= seq {
		return fmt.Errorf("event: fold: context_seq %d is not before assistant.message seq %d", e.ContextSeq, seq)
	}
	for _, call := range e.ToolCalls {
		if _, exists := s.PendingCalls[call.ID]; exists {
			return fmt.Errorf("event: fold: duplicate assistant call %s", call.ID)
		}
		if _, exists := s.InFlight[call.ID]; exists {
			return fmt.Errorf("event: fold: duplicate assistant call %s", call.ID)
		}
		if s.PendingCalls == nil {
			s.PendingCalls = make(map[string]ToolCall)
		}
		s.PendingCalls[call.ID] = call
	}
	return nil
}

func foldToolStarted(s *State, e ToolStarted) error {
	if err := requireTurn(s, e.TurnID); err != nil {
		return err
	}
	if _, exists := s.InFlight[e.CallID]; exists {
		return fmt.Errorf("event: fold: duplicate tool.started %s", e.CallID)
	}
	if s.InFlight == nil {
		s.InFlight = make(map[string]ToolState)
	}
	s.InFlight[e.CallID] = ToolState{CallID: e.CallID, Tool: e.Tool, Class: e.Class, Args: e.Args, ArgsBlob: e.ArgsBlob}
	delete(s.PendingCalls, e.CallID)
	if _, accepted := s.SessionAccepted[e.CallID]; accepted {
		delete(s.SessionAccepted, e.CallID)
		return foldGrant(s, grantSource{callID: e.CallID, tool: e.Tool, class: e.Class, args: e.Args, argsBlob: e.ArgsBlob})
	}
	return nil
}

func foldToolCompleted(s *State, e ToolCompleted) error {
	started, err := takeCompletedCall(s, e)
	if err != nil {
		return err
	}
	if err := foldSnapshots(s, e.Meta); err != nil {
		return err
	}
	if started.Tool == "todo" && e.OK {
		return foldTodoCompletion(s, started)
	}
	return nil
}

func foldTodoCompletion(s *State, started ToolState) error {
	if started.ArgsBlob != nil {
		s.Todos = nil
		s.TodoBlob = cloneBlob(started.ArgsBlob)
		return nil
	}
	s.TodoBlob = nil
	return foldTodos(s, started.Args)
}

func takeCompletedCall(s *State, e ToolCompleted) (ToolState, error) {
	if started, exists := s.InFlight[e.CallID]; exists {
		delete(s.InFlight, e.CallID)
		delete(s.DeclinedCalls, e.CallID)
		return started, nil
	}
	pending, exists := s.PendingCalls[e.CallID]
	if !exists && !scriptCall(s, e.CallID) {
		return ToolState{}, fmt.Errorf("event: fold: tool.completed without tool.started: %s", e.CallID)
	}
	started := ToolState{CallID: e.CallID, Tool: pending.Name, Args: pending.Args, ArgsBlob: pending.ArgsBlob}
	_, declined := s.DeclinedCalls[e.CallID]
	if e.OK || (e.Recovered != "skipped" && !declined) {
		return ToolState{}, fmt.Errorf("event: fold: unstarted call %s is neither skipped nor declined", e.CallID)
	}
	delete(s.PendingCalls, e.CallID)
	delete(s.DeclinedCalls, e.CallID)
	return started, nil
}

func scriptCall(s *State, callID string) bool {
	run, _, found := strings.CutLast(callID, "/")
	_, open := s.Scripts[run]
	return found && open
}

func foldApprovalRequested(s *State, e ApprovalRequested) error {
	if s.TurnID == "" {
		return errors.New("event: fold: approval outside a turn")
	}
	_, pending := s.PendingCalls[e.CallID]
	_, running := s.InFlight[e.CallID]
	if !pending && !running && !scriptCall(s, e.CallID) {
		return fmt.Errorf("event: fold: approval for unknown call %s", e.CallID)
	}
	if _, exists := s.Approvals[e.RequestID]; exists {
		return fmt.Errorf("event: fold: duplicate approval %s", e.RequestID)
	}
	if s.Approvals == nil {
		s.Approvals = make(map[string]ApprovalState)
	}
	s.Approvals[e.RequestID] = ApprovalState{CallID: e.CallID, Summary: e.Summary, Class: e.Class, GrantPrefix: e.GrantPrefix}
	return nil
}

func foldApprovalResolved(s *State, e ApprovalResolved) error {
	request, exists := s.Approvals[e.RequestID]
	if !exists {
		return fmt.Errorf("event: fold: approval.resolved without request: %s", e.RequestID)
	}
	delete(s.Approvals, e.RequestID)
	if e.Decision == "decline" || e.Decision == "cancel" {
		if s.DeclinedCalls == nil {
			s.DeclinedCalls = make(map[string]string)
		}
		s.DeclinedCalls[request.CallID] = e.Reason
	}
	if e.Decision != "accept_session" || e.By != "user" {
		return nil
	}
	source, known := grantSourceFor(s, request)
	if known {
		return foldGrant(s, source)
	}
	if s.SessionAccepted == nil {
		s.SessionAccepted = make(map[string]struct{})
	}
	s.SessionAccepted[request.CallID] = struct{}{}
	return nil
}

type grantSource struct {
	callID      string
	tool        string
	class       string
	args        jsontext.Value
	argsBlob    *BlobRef
	grantPrefix string
}

func grantSourceFor(s *State, request ApprovalState) (grantSource, bool) {
	call, pending := s.PendingCalls[request.CallID]
	if request.Class == "" || !pending {
		return grantSource{}, false
	}
	return grantSource{callID: call.ID, tool: call.Name, class: request.Class, args: call.Args, argsBlob: call.ArgsBlob, grantPrefix: request.GrantPrefix}, true
}

func foldGrant(s *State, source grantSource) error {
	if source.class == "exec" {
		return foldExecGrant(s, source)
	}
	if source.class != "edit" && source.class != "remote" {
		return nil
	}
	if s.Grants == nil {
		s.Grants = make(map[string]string)
	}
	s.Grants["tool:"+source.tool] = source.tool
	return nil
}

func foldExecGrant(s *State, source grantSource) error {
	prefix := source.grantPrefix
	if prefix == "" {
		if source.argsBlob != nil {
			return nil
		}
		fields, err := decodeObject(source.args)
		if err != nil {
			return fmt.Errorf("event: fold: exec grant arguments: %w", err)
		}
		command, err := decodeString(fields["command"])
		if err != nil {
			return fmt.Errorf("event: fold: exec grant command: %w", err)
		}
		var ok bool
		prefix, ok = rules.GrantPrefix(command)
		if !ok {
			return nil
		}
	}
	if s.Grants == nil {
		s.Grants = make(map[string]string)
	}
	s.Grants[prefix] = source.tool
	return nil
}

func foldStateEvent(s *State, seq uint64, e Event) error {
	switch value := e.(type) {
	case Compaction:
		copy := value
		s.LastCompaction = &copy
		return nil
	case ContextPruned:
		return requireTurn(s, value.TurnID)
	case ChildSpawned:
		return foldChild(s, value)
	case Branch:
		return foldBranch(s, seq, value.ParentSeq)
	default:
		return fmt.Errorf("event: fold: %T does not match %s", e, e.Type())
	}
}

func foldChild(s *State, e ChildSpawned) error {
	started, exists := s.InFlight[e.CallID]
	if (!exists || started.Class != "agent") && !scriptCall(s, e.CallID) {
		return fmt.Errorf("event: fold: child.spawned for unknown agent call %s", e.CallID)
	}
	if _, exists := s.Children[e.ChildID]; exists {
		return fmt.Errorf("event: fold: duplicate child %s", e.ChildID)
	}
	if s.Children == nil {
		s.Children = make(map[string]ChildState)
	}
	s.Children[e.ChildID] = ChildState{CallID: e.CallID, Runner: e.Runner}
	return nil
}

func foldBranch(s *State, seq, parent uint64) error {
	if s.TurnID != "" {
		return errors.New("event: fold: branch while a turn runs")
	}
	if parent >= seq {
		return errors.New("event: fold: branch parent must precede the branch")
	}
	if parent == 0 {
		return nil
	}
	for _, completedSeq := range s.CompletedTurns {
		if completedSeq == parent {
			return nil
		}
	}
	return fmt.Errorf("event: fold: unknown branch parent seq %d", parent)
}

func foldScriptEvent(s *State, e Event) error {
	switch value := e.(type) {
	case ScriptStarted:
		return foldScriptStarted(s, value)
	case ScriptEffect:
		return foldScriptEffect(s, value)
	case ScriptCompleted:
		return foldScriptCompleted(s, value)
	default:
		return fmt.Errorf("event: fold: %T does not match %s", e, e.Type())
	}
}

func foldScriptStarted(s *State, e ScriptStarted) error {
	if s.TurnID == "" {
		return errors.New("event: fold: script outside a turn")
	}
	if _, exists := s.Scripts[e.RunID]; exists {
		return fmt.Errorf("event: fold: duplicate script %s", e.RunID)
	}
	if s.Scripts == nil {
		s.Scripts = make(map[string]ScriptState)
	}
	s.Scripts[e.RunID] = ScriptState{}
	return nil
}

func foldScriptEffect(s *State, e ScriptEffect) error {
	run, exists := s.Scripts[e.RunID]
	if !exists {
		return fmt.Errorf("event: fold: unexpected script effect %s/%d", e.RunID, e.N)
	}
	if _, seen := run.Seen[e.N]; seen {
		return fmt.Errorf("event: fold: duplicate script effect %s/%d", e.RunID, e.N)
	}
	if run.Seen == nil {
		run.Seen = make(map[uint64]struct{})
	}
	run.Seen[e.N] = struct{}{}
	run.NextEffect = max(run.NextEffect, e.N+1)
	s.Scripts[e.RunID] = run
	return nil
}

func foldScriptCompleted(s *State, e ScriptCompleted) error {
	if _, exists := s.Scripts[e.RunID]; !exists {
		return fmt.Errorf("event: fold: script.completed without script.started: %s", e.RunID)
	}
	delete(s.Scripts, e.RunID)
	return nil
}

func NoteKey(note AdvisorNote) string {
	return note.Advisor + "\x00" + strconv.FormatUint(note.ReviewSeq, 10) + "\x00" + note.Note
}

func foldMetadata(s *State, seq uint64, e Event) error {
	switch value := e.(type) {
	case ThreadNamed:
		s.Name = value.Name
		return nil
	case ThreadArchived:
		s.Archived = value.Archived
		return nil
	default:
		return foldAdvisorNote(s, seq, e)
	}
}

func foldAdvisorNote(s *State, seq uint64, e Event) error {
	note, ok := e.(AdvisorNote)
	if !ok {
		return fmt.Errorf("event: fold: %T does not match %s", e, e.Type())
	}
	key := NoteKey(note)
	if _, exists := s.Notes[key]; exists {
		return fmt.Errorf("event: fold: duplicate advisor note from %s for review %d", note.Advisor, note.ReviewSeq)
	}
	if s.Notes == nil {
		s.Notes = make(map[string]uint64)
	}
	s.Notes[key] = seq
	return nil
}

func foldTodos(s *State, raw jsontext.Value) error {
	fields, err := decodeObject(raw)
	if err != nil {
		return fmt.Errorf("event: fold: todo arguments: %w", err)
	}
	var items []TodoItem
	r := objectReader{fields: fields}
	r.array("items", func(raw jsontext.Value) error {
		item, err := decodeTodoItem(raw)
		items = append(items, item)
		return err
	})
	if r.err != nil {
		return fmt.Errorf("event: fold: todo arguments: %w", r.err)
	}
	s.Todos = items
	return nil
}

func decodeTodoItem(raw jsontext.Value) (TodoItem, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return TodoItem{}, err
	}
	r := objectReader{fields: fields}
	item := TodoItem{Text: r.str("text"), Status: r.str("status")}
	if r.err != nil {
		return TodoItem{}, r.err
	}
	if item.Text == "" {
		return TodoItem{}, errors.New("todo text must not be empty")
	}
	if err := oneOf("todo status", item.Status, "pending", "in_progress", "done"); err != nil {
		return TodoItem{}, err
	}
	return item, nil
}

func foldSnapshots(s *State, meta jsontext.Value) error {
	trimmed := bytes.TrimSpace(meta)
	if len(trimmed) == 0 || trimmed[0] != '{' {
		return nil
	}
	fields, err := decodeObject(trimmed)
	if err != nil {
		return fmt.Errorf("event: fold: snapshot metadata: %w", err)
	}
	snapshots, err := snapshotMetadata(fields)
	if err != nil {
		return fmt.Errorf("event: fold: snapshot metadata: %w", err)
	}
	for _, snapshot := range snapshots {
		if s.Snapshots == nil {
			s.Snapshots = make(map[string]Snapshot)
		}
		s.Snapshots[snapshot.Ref] = snapshot
	}
	return nil
}

func snapshotMetadata(fields map[string]jsontext.Value) ([]Snapshot, error) {
	if hasSnapshotFields(fields) {
		snapshot, err := decodeSnapshot(fields)
		return []Snapshot{snapshot}, err
	}
	raw, exists := fields["captures"]
	if !exists {
		return nil, nil
	}
	var snapshots []Snapshot
	err := decodeArray(raw, func(raw jsontext.Value) error {
		fields, err := decodeObject(raw)
		if err != nil {
			return err
		}
		if !hasSnapshotFields(fields) {
			return nil
		}
		snapshot, err := decodeSnapshot(fields)
		snapshots = append(snapshots, snapshot)
		return err
	})
	return snapshots, err
}

func hasSnapshotFields(fields map[string]jsontext.Value) bool {
	_, ref := fields["ref"]
	_, path := fields["path"]
	_, blob := fields["blob"]
	return ref && path && blob
}

func decodeSnapshot(fields map[string]jsontext.Value) (Snapshot, error) {
	r := objectReader{fields: fields}
	var shown []LineRange
	r.array("shown", func(raw jsontext.Value) error {
		lineRange, err := decodeLineRange(raw)
		shown = append(shown, lineRange)
		return err
	})
	snapshot := Snapshot{
		Ref: r.str("ref"), Path: r.str("path"), Blob: r.str("blob"), Shown: shown,
		Revision: r.uint64("revision"), Epoch: r.uint64("epoch"),
	}
	if r.err != nil {
		return Snapshot{}, r.err
	}
	if snapshot.Ref == "" || snapshot.Path == "" {
		return Snapshot{}, errors.New("snapshot ref and path must not be empty")
	}
	if err := validSHA("snapshot blob", snapshot.Blob); err != nil {
		return Snapshot{}, err
	}
	return snapshot, nil
}

func decodeLineRange(raw jsontext.Value) (LineRange, error) {
	var values []int
	err := decodeArray(raw, func(raw jsontext.Value) error {
		value, err := decodeInt(raw)
		values = append(values, value)
		return err
	})
	if err != nil {
		return LineRange{}, err
	}
	if len(values) != 2 || values[0] < 1 || values[1] < values[0] {
		return LineRange{}, errors.New("shown range must contain an ascending positive line pair")
	}
	return LineRange{Start: values[0], End: values[1]}, nil
}
