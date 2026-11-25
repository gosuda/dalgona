//go:build gui

package decision

import (
	"fmt"
	"slices"

	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const draftLimit = 64

func (d *Dock) trim(head state.RequestKey) {
	for len(d.recent) > draftLimit {
		at := slices.IndexFunc(d.recent, func(k state.RequestKey) bool { return k != head && !d.cards[k].dirty() })
		if at < 0 {
			return
		}
		d.drop(d.recent[at])
	}
}

func (d *Dock) lock(c *card) {
	c.locked = ""
	if c.details.kind != kindQuestion || c.dirty() {
		return
	}
	oldest, held := d.oldestDraft(c)
	if held < draftLimit {
		return
	}
	c.locked = fmt.Sprintf("This window keeps unsent answers for %d requests. Send, skip, or dismiss one of them to answer here. The oldest is request %s in thread %s on %s.",
		draftLimit, oldest.key.Request, oldest.key.Thread.Thread, oldest.endpoint)
}

func (d *Dock) oldestDraft(except *card) (*card, int) {
	var oldest *card
	held := 0
	for _, c := range d.cards {
		if c == except || !c.dirty() {
			continue
		}
		held++
		if oldest == nil || c.serial < oldest.serial {
			oldest = c
		}
	}
	return oldest, held
}
