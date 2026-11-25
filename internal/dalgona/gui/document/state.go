package document

import "fmt"

type State struct {
	Scope     *Scope
	Selection Selection
	Selected  bool
	Focus     Position
	Focused   bool
	Anchor    Anchor
	Anchored  bool
	Pending   map[Key]uint64
}

type Anchor struct {
	Position     Position
	Displacement float32
}

func NewState(scope *Scope) *State {
	return &State{Scope: scope, Pending: map[Key]uint64{}}
}

func (st *State) Publish(change Change, doc *Document) error {
	if doc.header.Identity != change.To || doc.revision != change.Revision {
		return fmt.Errorf("%w: change for %+v revision %d does not describe %+v revision %d",
			ErrStale, change.To, change.Revision, doc.header.Identity, doc.revision)
	}
	i, ok := st.Scope.Index(change.From)
	if !ok {
		return fmt.Errorf("%w: %+v", ErrUnknown, change.From)
	}
	if !st.Scope.owns(change.To) {
		return fmt.Errorf("%w: %+v belongs to another endpoint or thread", ErrForeign, change.To)
	}
	next := *st
	if err := next.remapPositions(change, doc); err != nil {
		return err
	}
	if st.Selected && inverted(st.Selection, next.Selection, change.From) {
		next.Selection = Caret(next.Selection.Focus)
	}
	next.Pending = remapKeys(st.Pending, change.Keys)
	st.Scope.folds = remapKeys(st.Scope.folds, change.Keys)
	st.Scope.entries[i] = entry{ref: refOf(doc), doc: doc, used: st.Scope.clock + 1}
	st.Scope.clock++
	if change.From != change.To {
		delete(st.Scope.index, change.From)
		st.Scope.index[change.To] = i
	}
	*st = next
	return nil
}

func (st *State) remapPositions(change Change, doc *Document) error {
	targets := []struct {
		active   bool
		position *Position
	}{
		{st.Selected, &st.Selection.Anchor},
		{st.Selected, &st.Selection.Focus},
		{st.Focused, &st.Focus},
		{st.Anchored, &st.Anchor.Position},
	}
	for _, target := range targets {
		if !target.active || target.position.Document != change.From {
			continue
		}
		mapped, err := doc.Remap(change, *target.position)
		if err != nil {
			return err
		}
		*target.position = mapped
	}
	return nil
}

func inverted(before, after Selection, document Identity) bool {
	if before.Anchor.Document != document || before.Focus.Document != document {
		return false
	}
	was := before.Focus.Offset - before.Anchor.Offset
	now := after.Focus.Offset - after.Anchor.Offset
	return was > 0 && now < 0 || was < 0 && now > 0
}

func (st *State) Current(key Key, revision Revision) bool {
	i, ok := st.Scope.Index(key.Document)
	if !ok {
		return false
	}
	doc := st.Scope.entries[i].doc
	if doc == nil || doc.revision != revision {
		return false
	}
	at, ok := doc.blockAt(key.Span.Start)
	return ok && doc.blocks[at].Key == key
}
