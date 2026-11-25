//go:build gui

package composer

import (
	"strconv"
	"strings"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const queueLabel = "Queued follow-ups"

type clickable = widget.Clickable

type followUpQueue struct {
	edit   map[uint64]*clickable
	remove map[uint64]*clickable
}

func firstLine(text string) string {
	line, _, _ := strings.Cut(text, "\n")
	return line
}

func followUpSummary(f state.FollowUp) string {
	line := firstLine(f.Text)
	if n := len(f.Attachments); n > 0 {
		line += " · " + strconv.Itoa(n) + " attachments"
	}
	return line
}

func (q *followUpQueue) update(gtx layout.Context, d *Dock, env frame) {
	items := env.view.FollowUps(d.key)
	live := make(map[uint64]bool, len(items))
	for _, f := range items {
		live[f.ID] = true
		if c := q.edit[f.ID]; c != nil && c.Clicked(gtx) {
			env.Submit(state.EditFollowUp{Key: d.key, ID: f.ID})
			d.palette.reopen()
		}
		if c := q.remove[f.ID]; c != nil && c.Clicked(gtx) {
			env.Submit(state.RemoveFollowUp{Key: d.key, ID: f.ID})
		}
	}
	for id := range q.edit {
		if !live[id] {
			delete(q.edit, id)
			delete(q.remove, id)
		}
	}
}

func (q *followUpQueue) click(m map[uint64]*clickable, id uint64) *clickable {
	c := m[id]
	if c == nil {
		c = new(clickable)
		m[id] = c
	}
	return c
}

func (q *followUpQueue) layout(gtx layout.Context, d *Dock, env frame, look dockLook) layout.Dimensions {
	items := env.view.FollowUps(d.key)
	if len(items) == 0 {
		return layout.Dimensions{}
	}
	rows := make([]layout.Widget, 0, len(items))
	for _, f := range items {
		rows = append(rows, func(gtx layout.Context) layout.Dimensions {
			return q.item(gtx, env, look, f)
		})
	}
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
		return column(gtx, theme.Space2, rows...)
	})
	region(gtx, queueLabel, call, dims)
	return dims
}

func (q *followUpQueue) item(gtx layout.Context, env frame, look dockLook, f state.FollowUp) layout.Dimensions {
	summary := followUpSummary(f)
	edit := control{click: q.click(q.edit, f.ID), label: "Edit", describe: summary, variant: variantQuiet}
	remove := control{click: q.click(q.remove, f.ID), label: "Remove", describe: summary, variant: variantQuiet}
	return flow(gtx, theme.Space2,
		func(gtx layout.Context) layout.Dimensions {
			st := small(look.palette.Text)
			st.maxLines = 1
			gtx.Constraints.Max.X = max(0, gtx.Constraints.Max.X-dp(gtx, theme.TagMinWidth)*2)
			return drawText(gtx, env.Shaper, st, summary)
		},
		func(gtx layout.Context) layout.Dimensions {
			return edit.layout(gtx, env.Shaper, look.palette, look.point)
		},
		func(gtx layout.Context) layout.Dimensions {
			return remove.layout(gtx, env.Shaper, look.palette, look.point)
		},
	)
}
