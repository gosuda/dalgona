package remote

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"maps"
	"os"
	"slices"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/agent"
)

const localRunner = "local"

type RunnerStatus struct {
	Name         string
	Service      string
	Available    bool
	Capabilities []string
}

type BindingStatus struct {
	ThreadID        string
	RunnerID        string
	BoxID           string
	SharedThreadIDs []string
	State           string
	BindingRevision string
}

type Status struct {
	Runners        []RunnerStatus
	Bindings       []BindingStatus
	LedgerRevision string
}

func (b *Binder) Status(ctx context.Context, threadID string) (Status, error) {
	_, revision, err := b.gcState()
	if err != nil {
		return Status{}, err
	}
	bindings, err := b.bindings(threadID)
	if err != nil {
		return Status{}, err
	}
	return Status{Runners: b.runners(ctx), Bindings: bindings, LedgerRevision: revision}, nil
}

func (b *Binder) runners(ctx context.Context) []RunnerStatus {
	runners := []RunnerStatus{{Name: localRunner, Service: localRunner, Available: true, Capabilities: []string{"exec", "workspace"}}}
	for _, name := range slices.Sorted(maps.Keys(b.opts.Config.Runners)) {
		spec := b.opts.Config.Runners[name]
		_, err := b.services.Service(ctx, spec)
		runners = append(runners, RunnerStatus{Name: name, Service: spec.Service, Available: err == nil, Capabilities: []string{"exec", "pull", "workspace"}})
	}
	return runners
}

func (b *Binder) bindings(threadID string) ([]BindingStatus, error) {
	books, err := b.readLedger()
	if err != nil {
		return nil, err
	}
	live := map[boxKey][]string{}
	var bound []BindingStatus
	var keys []boxKey
	err = b.eachBound(func(id string, info threadInfo) bool {
		key := books.resolve(boxOf(*info.bound))
		if !info.archived {
			live[key] = append(live[key], id)
		}
		if threadID == "" || id == threadID {
			bound = append(bound, bindingOf(id, info, key, books))
			keys = append(keys, key)
		}
		return true
	})
	if err != nil {
		return nil, err
	}
	for i := range bound {
		bound[i].SharedThreadIDs = slices.DeleteFunc(slices.Clone(live[keys[i]]), func(id string) bool { return id == bound[i].ThreadID })
	}
	if threadID != "" && len(bound) == 0 {
		return b.unbound(threadID)
	}
	return bound, nil
}

func bindingOf(id string, info threadInfo, key boxKey, books ledger) BindingStatus {
	state := "bound"
	switch {
	case books.destroyed[key]:
		state = "destroyed"
	case info.archived:
		state = "archived"
	}
	return BindingStatus{
		ThreadID: id, RunnerID: info.created.Runner, BoxID: key.id, State: state,
		BindingRevision: agent.BindingRevision(info.created.Runner, info.bound),
	}
}

func (b *Binder) unbound(threadID string) ([]BindingStatus, error) {
	info, err := b.threadInfo(threadID)
	if err != nil {
		return nil, err
	}
	if info.created.ID == "" {
		return nil, ErrUnknownThread
	}
	return []BindingStatus{{
		ThreadID: threadID, RunnerID: info.created.Runner, SharedThreadIDs: []string{}, State: "unbound",
		BindingRevision: agent.BindingRevision(info.created.Runner, nil),
	}}, nil
}

var ErrUnknownThread = errors.New("remote: no such thread")

func (b *Binder) ledgerRevision(boxes []Box) (string, error) {
	data, err := os.ReadFile(b.ledgerPath())
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return "", err
	}
	digest := sha256.New()
	digest.Write(data)
	for _, box := range boxes {
		digest.Write([]byte(strings.Join(append([]string{box.Service, box.ID, box.Runner}, box.Threads...), "\x00") + "\n"))
	}
	return hex.EncodeToString(digest.Sum(nil)), nil
}
