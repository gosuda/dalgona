package agent

import (
	"context"
	"errors"
	"fmt"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/prompt"
	"github.com/gosuda/dalgona/internal/keywords"
)

type preparedInput struct {
	kind     InputKind
	text     string
	textBlob *event.BlobRef
	notices  []event.Notice
	mentions []event.Mention
}

func (t *Thread) Submit(ctx context.Context, in Input) error {
	prepared, err := t.prepareInput(in)
	if err != nil {
		return err
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	return askErr(t, func(a *actor) error { return a.submit(prepared) })
}

func (t *Thread) prepareInput(in Input) (preparedInput, error) {
	kind := in.Kind
	if kind == "" {
		kind = InputPrompt
	}
	masked := t.host.mask(in.Text)
	text, blob, err := t.host.textPayload(masked)
	if err != nil {
		return preparedInput{}, err
	}
	out := preparedInput{kind: kind, text: text, textBlob: blob}
	for _, word := range keywords.Match(in.Text, t.host.keywords, t.toolNames) {
		noticeText, noticeBlob, err := t.host.textPayload(word.Notice)
		if err != nil {
			return preparedInput{}, err
		}
		out.notices = append(out.notices, event.Notice{Word: word.Word, Text: noticeText, TextBlob: noticeBlob})
	}
	for _, mention := range prompt.Mentions(in.Text, t.created.Cwd) {
		mentionText, mentionBlob, err := t.host.textPayload(t.host.mask(mention.Text))
		if err != nil {
			return preparedInput{}, err
		}
		out.mentions = append(out.mentions, event.Mention{Path: mention.Path, Text: mentionText, TextBlob: mentionBlob})
	}
	return out, nil
}

func (a *actor) submit(in preparedInput) error {
	a.autoContinue = true
	message := event.UserMessage{
		ItemID: newID("item-"), Text: in.text, TextBlob: in.textBlob, Kind: string(in.kind),
		Notices: in.notices, Mentions: in.mentions,
	}
	if a.busy {
		if in.kind == InputPrompt || a.state.TurnID == "" {
			return runningError(a.t.id)
		}
		message.TurnID = a.state.TurnID
		_, err := a.append(message)
		return err
	}
	turnID := newID("turn-")
	message.TurnID, message.Kind = turnID, string(InputPrompt)
	if _, err := a.append(event.TurnStarted{TurnID: turnID}, message); err != nil {
		return err
	}
	a.spawnTurn(turnID)
	return nil
}

func (t *Thread) Interrupt() {
	if err := t.do(func(a *actor) { a.interrupt() }); err != nil {
		t.host.log.Debug("agent: interrupt after close", "thread", t.id)
	}
}

func (a *actor) interrupt() {
	a.autoContinue = false
	if a.cancel != nil {
		a.cancel(errInterrupted)
	}
}

func (t *Thread) Answer(requestID string, reply Reply) error {
	return askErr(t, func(a *actor) error {
		ch, ok := a.approvals[requestID]
		if !ok {
			return fmt.Errorf("%w: %s", ErrUnknownRequest, requestID)
		}
		delete(a.approvals, requestID)
		ch <- reply
		return nil
	})
}

func (t *Thread) Revert(beforeTurn string) error {
	return askErr(t, func(a *actor) error {
		if a.busy {
			return runningError(a.t.id)
		}
		parent, err := revertParent(event.ActivePath(a.records), beforeTurn)
		if err != nil {
			return err
		}
		_, err = a.append(event.Branch{ParentSeq: parent})
		return err
	})
}

func revertParent(path []event.Seqd, beforeTurn string) (uint64, error) {
	var parent uint64
	for _, item := range path {
		switch value := item.Event.(type) {
		case event.TurnStarted:
			if value.TurnID == beforeTurn {
				return parent, nil
			}
		case event.TurnCompleted:
			parent = item.Seq
		}
	}
	return 0, notOnPathError(beforeTurn)
}

func (t *Thread) SetLeaf(turn string) error {
	return askErr(t, func(a *actor) error {
		if a.busy {
			return runningError(a.t.id)
		}
		seq, ok := a.state.CompletedTurns[turn]
		if !ok {
			return unknownTurnError(turn)
		}
		_, err := a.append(event.Branch{ParentSeq: seq})
		return err
	})
}

type forkSource struct {
	created event.SessionCreated
	bound   *event.RunnerBound
	events  []event.Seqd
	err     error
}

func (t *Thread) Fork(ctx context.Context, lastTurn string) (*Thread, error) {
	source, err := ask(t, func(a *actor) forkSource { return a.forkSource(lastTurn) })
	if err != nil {
		return nil, err
	}
	if source.err != nil {
		return nil, source.err
	}
	created := source.created
	created.ID, created.ForkOf, created.Parent, created.Advisor = t.host.newThreadID(), t.id, "", ""
	child, err := t.host.openThread(ctx, loadedSession{created: created}, Overrides{})
	if err != nil {
		return nil, err
	}
	batch := forkBatch(created, source)
	if _, err := child.appendEvents(ctx, batch...); err != nil {
		return nil, errors.Join(err, child.Close())
	}
	t.host.registerSession(created, uint64(len(batch)))
	return child, nil
}

func (a *actor) forkSource(lastTurn string) forkSource {
	if a.busy {
		return forkSource{err: runningError(a.t.id)}
	}
	path := event.ActivePath(a.records)
	end := len(path)
	if lastTurn != "" {
		end = forkEnd(path, a.state.CompletedTurns[lastTurn])
		if end < 0 {
			return forkSource{err: notOnPathError(lastTurn)}
		}
	}
	return forkSource{created: a.t.created, bound: a.state.Runner, events: path[:end]}
}

func forkEnd(path []event.Seqd, completedSeq uint64) int {
	for i, item := range path {
		if completedSeq != 0 && item.Seq == completedSeq {
			return i + 1
		}
	}
	return -1
}

func forkBatch(created event.SessionCreated, source forkSource) []event.Event {
	batch := make([]event.Event, 0, len(source.events)+2)
	batch = append(batch, created)
	if source.bound != nil {
		batch = append(batch, *source.bound)
	}
	renumbered := make(map[uint64]uint64, len(source.events))
	for _, item := range source.events {
		switch value := item.Event.(type) {
		case event.SessionCreated, event.RunnerBound:
			continue
		case event.Compaction:
			value.FirstKeptSeq = firstKeptAfter(renumbered, value.FirstKeptSeq, uint64(len(batch))+1)
			batch = append(batch, value)
		default:
			batch = append(batch, item.Event)
		}
		renumbered[item.Seq] = uint64(len(batch))
	}
	return batch
}

func firstKeptAfter(renumbered map[uint64]uint64, old, fallback uint64) uint64 {
	best := fallback
	for from, to := range renumbered {
		if from >= old && to < best {
			best = to
		}
	}
	return best
}

func (h *Host) registerSession(created event.SessionCreated, head uint64) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.sessions[created.ID] = infoFrom(created, head, false)
}

func (t *Thread) Compact(ctx context.Context) error {
	done, err := ask(t, func(a *actor) chan error { return a.spawnCompaction() })
	if err != nil {
		return err
	}
	select {
	case err := <-done:
		return err
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (a *actor) spawnCompaction() chan error {
	done := make(chan error, 1)
	if a.busy {
		done <- runningError(a.t.id)
		return done
	}
	a.spawn(func(ctx context.Context) {
		w := &worker{t: a.t}
		err := w.openRegistry()
		if err == nil {
			err = w.compact(ctx, compactManual)
		}
		done <- err
		w.release()
	})
	return done
}
