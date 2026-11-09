package remote

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

// Box is a journaled box with no unarchived thread bound to it.
type Box struct {
	Service string
	ID      string
	Runner  string
	Threads []string
}

type boxKey struct{ service, id string }

func boxOf(bound event.RunnerBound) boxKey { return boxKey{bound.Service, bound.BoxID} }

// Archive destroys the thread's box unless another unarchived thread's latest
// runner.bound names it. thread/archive calls it before it journals
// thread.archived.
func (b *Binder) Archive(ctx context.Context, threadID string) error {
	info, err := b.threadInfo(threadID)
	if err != nil || info.bound == nil {
		return err
	}
	books, err := b.readLedger()
	if err != nil {
		return err
	}
	key := books.resolve(boxOf(*info.bound))
	user, err := b.otherUser(books, threadID, key)
	if err != nil {
		return err
	}
	if user != "" {
		b.log.Info(fmt.Sprintf("runner: box %s kept; still used by thread %s", info.bound.BoxID, user))
		return nil
	}
	return b.destroy(ctx, info.bound.Runner, key)
}

func (b *Binder) otherUser(books ledger, threadID string, key boxKey) (string, error) {
	var user string
	err := b.eachBound(func(id string, info threadInfo) bool {
		if id != threadID && !info.archived && books.resolve(boxOf(*info.bound)) == key {
			user = id
			return false
		}
		return true
	})
	return user, err
}

func (b *Binder) eachBound(fn func(threadID string, info threadInfo) bool) error {
	if b.opts.Journal == nil {
		return errors.New("remote: thread operations need the journal")
	}
	sessions, err := b.opts.Journal.Sessions()
	if err != nil {
		return fmt.Errorf("remote: list threads: %w", err)
	}
	for _, head := range sessions {
		info, err := b.threadInfo(head.ID)
		if err != nil {
			return err
		}
		if info.bound != nil && !fn(head.ID, info) {
			return nil
		}
	}
	return nil
}

// GC lists boxes whose bound threads are all archived, asks through
// Options.Confirm, and destroys them on yes. It returns the listed boxes.
func (b *Binder) GC(ctx context.Context) ([]Box, error) {
	boxes, err := b.unboundBoxes()
	if err != nil || len(boxes) == 0 {
		return boxes, err
	}
	for _, box := range boxes {
		b.note("%s %s (runner %s; archived threads %s)", box.Service, box.ID, box.Runner, strings.Join(box.Threads, ", "))
	}
	ok, err := b.confirm(fmt.Sprintf("destroy %d unbound boxes?", len(boxes)))
	if err != nil || !ok {
		return boxes, err
	}
	var errs []error
	for _, box := range boxes {
		errs = append(errs, b.destroy(ctx, box.Runner, boxKey{box.Service, box.ID}))
	}
	return boxes, errors.Join(errs...)
}

func (b *Binder) unboundBoxes() ([]Box, error) {
	books, err := b.readLedger()
	if err != nil {
		return nil, err
	}
	live := map[boxKey]bool{}
	found := map[boxKey]*Box{}
	var order []boxKey
	err = b.eachBound(func(id string, info threadInfo) bool {
		key := books.resolve(boxOf(*info.bound))
		if !info.archived {
			live[key] = true
			return true
		}
		if found[key] == nil {
			found[key] = &Box{Service: key.service, ID: key.id, Runner: info.bound.Runner}
			order = append(order, key)
		}
		found[key].Threads = append(found[key].Threads, id)
		return true
	})
	var boxes []Box
	for _, key := range order {
		if !live[key] && !books.destroyed[key] {
			boxes = append(boxes, *found[key])
		}
	}
	return boxes, err
}

func (b *Binder) destroy(ctx context.Context, runnerName string, key boxKey) error {
	svc, err := b.service(ctx, b.runnerSpec(event.RunnerBound{Runner: runnerName, Service: key.service}))
	if err != nil {
		return err
	}
	if err := svc.Destroy(ctx, key.id); err != nil {
		if sandboxsvc.IsGone(err) {
			return b.appendLedger("destroyed", key.service, key.id)
		}
		return fmt.Errorf("remote: destroy box %s: %w", key.id, err)
	}
	return b.appendLedger("destroyed", key.service, key.id)
}

func (b *Binder) replaced(ctx context.Context, threadID string, current event.RunnerBound, oldID string) error {
	if err := b.appendLedger("replaced", current.Service, oldID, current.BoxID); err != nil {
		return err
	}
	books, err := b.readLedger()
	if err != nil {
		return err
	}
	key := boxOf(current)
	return b.eachBound(func(id string, info threadInfo) bool {
		other := *info.bound
		if id != threadID && !info.archived && other.BoxID != current.BoxID && books.resolve(boxOf(other)) == key {
			other.BoxID = current.BoxID
			b.recordShared(ctx, id, other)
		}
		return true
	})
}

func (b *Binder) recordShared(ctx context.Context, threadID string, bound event.RunnerBound) {
	if err := b.record(ctx, threadID, bound); err != nil {
		b.log.Info("remote: shared box binding not recorded; the thread resolves it on its next bind",
			slog.String("thread", threadID), slog.String("box", bound.BoxID), slog.Any("error", err))
	}
}

type ledger struct {
	destroyed map[boxKey]bool
	replaced  map[boxKey]string
}

func (l ledger) resolve(key boxKey) boxKey {
	for range len(l.replaced) {
		next, ok := l.replaced[key]
		if !ok {
			break
		}
		key.id = next
	}
	return key
}

func (b *Binder) ledgerPath() string { return filepath.Join(b.opts.Config.Home, "boxes.log") }

func (b *Binder) readLedger() (ledger, error) {
	books := ledger{destroyed: map[boxKey]bool{}, replaced: map[boxKey]string{}}
	file, err := os.Open(b.ledgerPath())
	if errors.Is(err, os.ErrNotExist) {
		return books, nil
	}
	if err != nil {
		return books, fmt.Errorf("remote: read box ledger: %w", err)
	}
	defer func() { _ = file.Close() }()
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		books.add(strings.Fields(scanner.Text()))
	}
	return books, scanner.Err()
}

func (l ledger) add(fields []string) {
	switch {
	case len(fields) == 3 && fields[0] == "destroyed":
		l.destroyed[boxKey{fields[1], fields[2]}] = true
	case len(fields) == 4 && fields[0] == "replaced":
		l.replaced[boxKey{fields[1], fields[2]}] = fields[3]
	}
}

func (b *Binder) appendLedger(fields ...string) error {
	b.ledgerMu.Lock()
	defer b.ledgerMu.Unlock()
	file, err := os.OpenFile(b.ledgerPath(), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return fmt.Errorf("remote: open box ledger: %w", err)
	}
	_, writeErr := fmt.Fprintln(file, strings.Join(fields, " "))
	return errors.Join(writeErr, file.Sync(), file.Close())
}
