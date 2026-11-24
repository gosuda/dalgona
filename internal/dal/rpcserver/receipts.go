package rpcserver

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io/fs"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

const (
	receiptLifetime     = 25 * time.Hour
	receiptLimit        = 10000
	receiptBytesLimit   = 32 << 20
	receiptOutcomeSpace = resultBytes + len(`,"result":`) + len(stateSucceeded) - len(stateReserved)
	receiptDirName      = "receipts"
	receiptSuffix       = ".json"
	stateReserved       = "reserved"
	stateRunning        = "running"
	stateSucceeded      = "succeeded"
	stateFailed         = "failed"
	stateUnknown        = "unknown"
	operationIDMember   = "operationId"
)

var coveredMutations = []string{
	"thread/start", "thread/name/set", "thread/archive", "thread/unarchive", "thread/compact/start", "thread/revert", "thread/fork",
	"turn/start", "turn/steer", "turn/interrupt", "review/start", "dalgona/thread/leaf/set", "dalgona/advisor/set", "dalgona/advisor/reset",
	"config/value/write", "config/batchWrite", "dalgona/remote/pull", "dalgona/remote/gc",
}

var (
	operationNamesMu sync.Mutex
	operationNames   = map[string]bool{}
	receiptStoresMu  sync.Mutex
	receiptStores    = map[string]*receiptStore{}
)

type operation struct {
	method   string
	id       string
	digest   string
	threadID string
}

type receipt struct {
	OperationID string         `json:"operationId"`
	Method      string         `json:"method"`
	Digest      string         `json:"digest"`
	ThreadID    string         `json:"threadId,omitzero"`
	State       string         `json:"state"`
	Result      jsontext.Value `json:"result,omitzero"`
	Error       *rpc.RPCError  `json:"error,omitzero"`
	ExpiresAt   time.Time      `json:"expiresAt"`
	charge      int
	damaged     bool
}

type journalLookup func(rec receipt) (receipt, bool)

const (
	methodThreadStart = "thread/start"
	methodThreadFork  = "thread/fork"
)

var errUnboundReceipt = errors.New("rpcserver: receipt names no journaled thread")

type journaledEffect struct {
	thread *agent.Thread
	event  event.Event
}

type journalBinding struct {
	effect func(event.Event) bool
	result func(*server, journaledEffect) (jsontext.Value, error)
}

var journalBindings = map[string]journalBinding{
	methodThreadStart:         {journaledAs(event.TypeSessionCreated), (*server).startedResult},
	methodThreadFork:          {journaledAs(event.TypeSessionCreated), (*server).startedResult},
	"thread/name/set":         {journaledAs(event.TypeThreadNamed), emptyResult},
	"thread/archive":          {archived, emptyResult},
	"thread/unarchive":        {unarchived, (*server).threadResult},
	"thread/revert":           {journaledAs(event.TypeBranch), (*server).threadResult},
	"dalgona/thread/leaf/set": {journaledAs(event.TypeBranch), emptyResult},
	"thread/compact/start":    {journaledAs(event.TypeCompaction), emptyResult},
	"turn/start":              {journaledAs(event.TypeUserMessage), turnStarted},
	"turn/steer":              {journaledAs(event.TypeUserMessage), turnSteered},
	"turn/interrupt":          {journaledAs(event.TypeTurnCompleted), emptyResult},
	"review/start":            {journaledAs(event.TypeUserMessage), reviewStarted},
}

func journaledAs(kind string) func(event.Event) bool {
	return func(e event.Event) bool { return e.Type() == kind }
}

func archived(e event.Event) bool {
	value, ok := e.(event.ThreadArchived)
	return ok && value.Archived
}

func unarchived(e event.Event) bool {
	value, ok := e.(event.ThreadArchived)
	return ok && !value.Archived
}

func emptyResult(*server, journaledEffect) (jsontext.Value, error) { return emptyObject(), nil }

func turnStarted(_ *server, effect journaledEffect) (jsontext.Value, error) {
	message, _ := effect.event.(event.UserMessage)
	return rpc.Marshal(rpc.TurnStartResponse{Turn: rpc.Turn{ID: message.TurnID, Status: rpc.StatusInProgress}})
}

func turnSteered(_ *server, effect journaledEffect) (jsontext.Value, error) {
	message, _ := effect.event.(event.UserMessage)
	return rpc.Marshal(rpc.TurnSteerResponse{TurnID: message.TurnID})
}

func reviewStarted(_ *server, effect journaledEffect) (jsontext.Value, error) {
	message, _ := effect.event.(event.UserMessage)
	return rpc.Marshal(rpc.ReviewStartResponse{ReviewThreadID: effect.thread.ID(), Turn: rpc.Turn{ID: message.TurnID, Status: rpc.StatusInProgress}})
}

type receiptStore struct {
	dir      string
	mu       sync.Mutex
	receipts map[string]*receipt
	bytes    int
}

func operationMethods() map[string]method {
	return map[string]method{
		"dalgona/operation/read": deadline(MetadataDeadline, (*server).operationRead),
	}
}

func coverMutations(table map[string]method) {
	for _, name := range coveredMutations {
		if m, ok := table[name]; ok {
			table[name] = receiptCovered(name, m)
		}
	}
}

func withOperation(name string, m method) method {
	operationNamesMu.Lock()
	operationNames[name] = true
	operationNamesMu.Unlock()
	return requireOperation(name, m)
}

func requireOperation(name string, m method) method {
	return func(s *server, ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
		op, err := operationOf(name, operationIDMember, params)
		if err != nil {
			return nil, err
		}
		if op.id == "" {
			return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
		}
		return s.runOperation(ctx, op, func(ctx context.Context) (jsontext.Value, error) { return m(s, ctx, params) })
	}
}

func receiptCovered(name string, m method) method {
	return func(s *server, ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
		op, err := operationOf(name, rpc.OperationMember, params)
		if err != nil {
			return nil, err
		}
		if op.id == "" {
			return m(s, ctx, params)
		}
		if s.native.Load() == nil {
			return nil, invalidParams(rpc.OperationMember + " requires the dalgonaNative capability")
		}
		return s.runOperation(ctx, op, func(ctx context.Context) (jsontext.Value, error) { return m(s, ctx, params) })
	}
}

func (s *server) extension(name string) (method, bool) {
	if extension, ok := s.opts.OperationExtensions[name]; ok {
		effect := requireOperation(name, extensionMethod(extension))
		if live, ok := s.opts.OperationResults[name]; ok {
			return liveOperationResult(effect, live), true
		}
		return effect, true
	}
	extension, ok := s.opts.Extensions[name]
	switch {
	case !ok:
		return nil, false
	case slices.Contains(coveredMutations, name):
		return receiptCovered(name, extensionMethod(extension)), true
	}
	return extensionMethod(extension), true
}

func extensionMethod(extension Extension) method {
	return func(_ *server, ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
		result, err := extension(ctx, params)
		if err != nil {
			return nil, internalError(err)
		}
		return result, nil
	}
}

func operationOf(name, member string, params jsontext.Value) (operation, error) {
	op := operation{method: name}
	_, journaled := journalBindings[name]
	err := decodeParams(params, func(key string, d *jsontext.Decoder) error {
		var target *string
		switch {
		case key == member:
			target = &op.id
		case journaled && key == "threadId":
			target = &op.threadID
		default:
			return d.SkipValue()
		}
		value, readErr := rpc.ReadString(d)
		*target = value
		return readErr
	})
	if err != nil {
		return operation{}, err
	}
	if op.id == "" {
		return op, nil
	}
	op.digest, err = paramDigest(params)
	return op, err
}

func paramDigest(params jsontext.Value) (string, error) {
	canonical := jsontext.Value(bytes.Clone(params))
	if err := canonical.Canonicalize(); err != nil {
		return "", invalidParams("params cannot be canonicalized: " + err.Error())
	}
	sum := sha256.Sum256(canonical)
	return hex.EncodeToString(sum[:]), nil
}

func (s *server) runOperation(ctx context.Context, op operation, effect func(context.Context) (jsontext.Value, error)) (jsontext.Value, error) {
	store, err := s.receipts()
	if err != nil {
		s.log.Warn("rpc: open receipt store", "error", err)
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	r, replay, err := store.reserve(op, time.Now())
	if err != nil {
		return nil, err
	}
	if replay {
		return r.Result, recordedError(r.Error)
	}
	store.mark(r, stateRunning)
	result, effectErr := effect(agent.WithOperation(ctx, op.id))
	if err := store.finish(r, result, effectErr, ctx.Err() != nil); err != nil {
		s.log.Warn("rpc: record operation outcome", "operation", op.id, "error", err)
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorUnknownOutcome, OperationID: op.id})
	}
	return result, effectErr
}

func recordedError(recorded *rpc.RPCError) error {
	if recorded == nil {
		return nil
	}
	return recorded
}

func (s *server) receipts() (*receiptStore, error) {
	return receiptsFor(s.host.Config().Home, s.journaledOutcome)
}

func (s *server) journaledOutcome(rec receipt) (receipt, bool) {
	binding, journaled := journalBindings[rec.Method]
	if !journaled {
		return rec, false
	}
	effect, found, err := s.locateEffect(rec)
	if err != nil {
		return rec, false
	}
	if !found || !binding.effect(effect.event) {
		rec.State, rec.Error = stateFailed, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorCancelled, OperationID: rec.OperationID})
		return rec, true
	}
	result, err := binding.result(s, effect)
	if err != nil {
		return rec, false
	}
	rec.State, rec.Result = stateSucceeded, result
	return rec, true
}

func (s *server) locateEffect(rec receipt) (journaledEffect, bool, error) {
	if rec.Method == methodThreadStart || rec.Method == methodThreadFork {
		return s.createdThread(rec.OperationID)
	}
	if rec.ThreadID == "" {
		return journaledEffect{}, false, errUnboundReceipt
	}
	thread, err := s.host.Resume(s.ctx, rec.ThreadID)
	if err != nil {
		return journaledEffect{}, false, err
	}
	records, _, err := thread.Records()
	if err != nil {
		return journaledEffect{}, false, err
	}
	effect, found := effectIn(records, rec.OperationID)
	return journaledEffect{thread: thread, event: effect}, found, nil
}

func (s *server) createdThread(operationID string) (journaledEffect, bool, error) {
	reserved, err := rpc.OperationTime(operationID)
	if err != nil {
		return journaledEffect{}, false, err
	}
	for _, info := range s.host.AllThreads() {
		if info.Created.Before(reserved.Add(-rpc.OperationFuture)) {
			continue
		}
		thread, err := s.host.Resume(s.ctx, info.ID)
		if err != nil {
			return journaledEffect{}, false, err
		}
		records, _, err := thread.Records()
		if err != nil {
			return journaledEffect{}, false, err
		}
		if created, ok := sessionCreated(records); ok && created.Operation == operationID {
			return journaledEffect{thread: thread, event: created}, true, nil
		}
	}
	return journaledEffect{}, false, nil
}

func (s *server) startedResult(effect journaledEffect) (jsontext.Value, error) {
	snap, err := s.snapshot(effect.thread, (*tracker).replayLive)
	if err != nil {
		return nil, err
	}
	response, err := s.session(effect.thread, snap)
	if err != nil {
		return nil, err
	}
	return rpc.Marshal(response)
}

func (s *server) threadResult(effect journaledEffect) (jsontext.Value, error) {
	info, ok := s.info(effect.thread.ID())
	if !ok {
		return nil, unknownThread(effect.thread.ID())
	}
	return rpc.Marshal(rpc.ThreadReadResponse{Thread: s.threadObject(info, threadStatus(effect.thread), nil)})
}

func effectIn(records []event.Seqd, operationID string) (event.Event, bool) {
	for _, record := range slices.Backward(records) {
		if event.OperationOf(record.Event) == operationID {
			return record.Event, true
		}
	}
	return nil, false
}

func receiptsFor(home string, journal journalLookup) (*receiptStore, error) {
	dir := filepath.Join(home, receiptDirName)
	receiptStoresMu.Lock()
	defer receiptStoresMu.Unlock()
	if store, ok := receiptStores[dir]; ok {
		return store, nil
	}
	store := &receiptStore{dir: dir, receipts: map[string]*receipt{}}
	if err := store.load(time.Now(), journal); err != nil {
		return nil, err
	}
	receiptStores[dir] = store
	return store, nil
}

func (r *receiptStore) load(now time.Time, journal journalLookup) error {
	if err := prepareReceiptDirectory(r.dir); err != nil {
		return err
	}
	entries, err := os.ReadDir(r.dir)
	if err != nil {
		return fmt.Errorf("list receipts: %w", err)
	}
	for _, entry := range entries {
		if !strings.HasSuffix(entry.Name(), receiptSuffix) {
			continue
		}
		if err := r.loadOne(filepath.Join(r.dir, entry.Name()), now, journal); err != nil {
			return err
		}
	}
	return nil
}

func (r *receiptStore) loadOne(path string, now time.Time, journal journalLookup) error {
	raw, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("read receipt %s: %w", path, err)
	}
	var rec receipt
	if err := json.Unmarshal(raw, &rec); err != nil || rec.OperationID+receiptSuffix != filepath.Base(path) {
		r.guardDamaged(strings.TrimSuffix(filepath.Base(path), receiptSuffix), len(raw))
		return nil
	}
	if !now.Before(rec.ExpiresAt) {
		return removeReceipt(path)
	}
	rec.charge = len(raw)
	if rec.State == stateReserved || rec.State == stateRunning {
		if err := r.recoverOutcome(&rec, journal); err != nil {
			return err
		}
	}
	r.receipts[rec.OperationID] = &rec
	r.bytes += rec.charge
	return nil
}

func (r *receiptStore) recoverOutcome(rec *receipt, journal journalLookup) error {
	next := interrupted(*rec, journal)
	limit := rec.charge + receiptOutcomeSpace
	err := r.persistOutcome(limit, &next)
	if data, ok := rpc.ErrorDataOf(err); ok && data.Kind == rpc.ErrorLimitExceeded {
		next = *rec
		next.State = stateUnknown
		err = r.persistOutcome(limit, &next)
	}
	if err != nil {
		return err
	}
	*rec = next
	return nil
}

func interrupted(rec receipt, journal journalLookup) receipt {
	if settled, ok := journal(rec); ok {
		return settled
	}
	rec.State = stateUnknown
	return rec
}

func (r *receiptStore) guardDamaged(id string, size int) {
	created, err := rpc.OperationTime(id)
	if err != nil {
		return
	}
	r.receipts[id] = &receipt{OperationID: id, State: stateUnknown, ExpiresAt: created.Add(receiptLifetime), charge: size, damaged: true}
	r.bytes += size
}

func removeReceipt(path string) error {
	if err := os.Remove(path); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("remove expired receipt: %w", err)
	}
	return nil
}

func (r *receiptStore) reserve(op operation, now time.Time) (*receipt, bool, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.pruneLocked(now)
	if existing, ok := r.receipts[op.id]; ok {
		return existing, true, existing.repeat(op)
	}
	if err := admitOperationID(op.id, now); err != nil {
		return nil, false, err
	}
	if len(r.receipts) >= receiptLimit {
		return nil, false, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	rec := &receipt{OperationID: op.id, Method: op.method, Digest: op.digest, ThreadID: op.threadID, State: stateReserved, ExpiresAt: now.Add(receiptLifetime)}
	raw, err := encodeReceipt(rec)
	if err != nil {
		return nil, false, errors.Join(rpc.NativeKind(rpc.ErrorUnavailable), err)
	}
	rec.charge = len(raw) + receiptOutcomeSpace
	if rec.charge > receiptBytesLimit-r.bytes {
		return nil, false, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	if err := r.persistBytes(rec.OperationID, raw); err != nil {
		return nil, false, errors.Join(rpc.NativeKind(rpc.ErrorUnavailable), err)
	}
	r.receipts[op.id] = rec
	r.bytes += rec.charge
	return rec, false, nil
}

func (rec *receipt) repeat(op operation) error {
	if rec.damaged {
		return rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorUnknownOutcome, OperationID: op.id})
	}
	if rec.Method != op.method || rec.Digest != op.digest {
		return rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorOperationConflict, OperationID: op.id})
	}
	switch rec.State {
	case stateSucceeded, stateFailed:
		return nil
	case stateUnknown:
		return rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorUnknownOutcome, OperationID: op.id})
	}
	return rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorBusy, OperationID: op.id})
}

func admitOperationID(id string, now time.Time) error {
	created, err := rpc.OperationTime(id)
	if err != nil || created.Before(now.Add(-rpc.OperationPast)) || created.After(now.Add(rpc.OperationFuture)) {
		return rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorInvalidParams, OperationID: id})
	}
	return nil
}

func (r *receiptStore) pruneLocked(now time.Time) {
	for id, rec := range r.receipts {
		if now.Before(rec.ExpiresAt) || rec.State == stateReserved || rec.State == stateRunning {
			continue
		}
		if removeReceipt(r.path(id)) == nil {
			delete(r.receipts, id)
			r.bytes -= rec.charge
		}
	}
}

func (r *receiptStore) mark(rec *receipt, state string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	rec.State = state
}

func (r *receiptStore) finish(rec *receipt, result jsontext.Value, effectErr error, cancelled bool) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	next := *rec
	switch {
	case effectErr == nil:
		next.State, next.Result = stateSucceeded, bytes.Clone(result)
	case cancelled:
		next.State = stateUnknown
	default:
		coded, _ := errors.AsType[*rpc.RPCError](internalError(effectErr))
		next.State, next.Error = stateFailed, coded
	}
	if err := r.persistOutcome(rec.charge, &next); err != nil {
		rec.State = stateUnknown
		return err
	}
	r.bytes += next.charge - rec.charge
	*rec = next
	return nil
}

func (r *receiptStore) persistOutcome(limit int, next *receipt) error {
	raw, err := encodeReceipt(next)
	if err != nil {
		return err
	}
	if len(raw) > limit {
		return rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	if err := r.persistBytes(next.OperationID, raw); err != nil {
		return err
	}
	next.charge = len(raw)
	return nil
}

func (r *receiptStore) path(id string) string { return filepath.Join(r.dir, id+receiptSuffix) }

func encodeReceipt(rec *receipt) ([]byte, error) {
	raw, err := json.Marshal(rec)
	if err != nil {
		return nil, fmt.Errorf("encode receipt: %w", err)
	}
	return raw, nil
}

func (r *receiptStore) persistBytes(id string, raw []byte) error {
	tmp, err := os.CreateTemp(r.dir, ".receipt-*")
	if err != nil {
		return fmt.Errorf("create receipt: %w", err)
	}
	_, writeErr := tmp.Write(raw)
	syncErr := tmp.Sync()
	closeErr := tmp.Close()
	if err := errors.Join(writeErr, syncErr, closeErr); err != nil {
		return errors.Join(fmt.Errorf("write receipt: %w", err), os.Remove(tmp.Name()))
	}
	if err := publishReceipt(tmp.Name(), r.path(id)); err != nil {
		return errors.Join(fmt.Errorf("publish receipt: %w", err), removeReceipt(tmp.Name()))
	}
	return nil
}

func (r *receiptStore) lookup(id string) (receipt, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	rec, ok := r.receipts[id]
	if !ok || !time.Now().Before(rec.ExpiresAt) {
		return receipt{}, false
	}
	return *rec, true
}

func (s *server) operationRead(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	var id string
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		if name != operationIDMember {
			return d.SkipValue()
		}
		value, readErr := rpc.ReadString(d)
		id = value
		return readErr
	})
	if err != nil {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if _, err = rpc.OperationTime(id); err != nil {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	store, err := s.receipts()
	if err != nil {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	rec, ok := store.lookup(id)
	if !ok {
		return nil, rpc.NativeKind(rpc.ErrorNotFound)
	}
	raw, err := json.Marshal(rpc.OperationRecord{State: rec.State, Method: rec.Method, Result: rec.Result, Error: rec.Error, ExpiresAt: rpc.FormatTimestamp(rec.ExpiresAt)})
	if err != nil {
		return nil, internalError(err)
	}
	return raw, nil
}

func (s *server) receiptMethods() []string {
	names := map[string]bool{}
	for _, name := range coveredMutations {
		if s.registered(name) {
			names[name] = true
		}
	}
	operationNamesMu.Lock()
	for name := range operationNames {
		if s.registered(name) {
			names[name] = true
		}
	}
	operationNamesMu.Unlock()
	for name := range s.opts.OperationExtensions {
		names[name] = true
	}
	return slices.Sorted(maps.Keys(names))
}
