//go:build gui

package shell

import (
	"fmt"
	"image"
	"math"
	"slices"
	"strings"

	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/text"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	glyphFolder   = "\U000F0256"
	glyphQueue    = "\U000F051F"
	glyphSandbox  = "\U000F03D7"
	glyphNoBox    = "\U000F002A"
	moonFirst     = 0xE38D
	moonSteps     = 14
	chooseModel   = "Choose a model"
	statusRoute   = "status"
	modelRoute    = "model"
	markBandSize  = 20
	bandItemCount = 8
)

var (
	effortGlyphs   = [...]string{"\U000F0873", "\U000F0875", "\U000F029A", "\U000F0874"}
	approvalGlyphs = map[string]string{"ask": "\U000F0E47", "auto": "\U000F167A", "yolo": "\U000F0238"}
)

type bandID uint8

const (
	bandModel bandID = iota
	bandEffort
	bandApproval
	bandSandbox
	bandFolder
	bandQueue
	bandContext
	bandMark
)

var bandNames = [...]string{"model", "effort", "approval", "sandbox", "folder", "queue", "context", "mark"}

var bandDrops = map[bandID]theme.Dp{
	bandFolder: theme.StatusDropFolderWidth,
	bandQueue:  theme.StatusDropQueueWidth,
	bandEffort: theme.StatusDropEffortWidth,
	bandMark:   theme.StatusDropMarkWidth,
}

type bandItem struct {
	id     bandID
	glyph  string
	label  string
	count  string
	detail string
}

type bandValues struct {
	model    string
	live     bool
	items    []bandItem
	detailOf map[bandID]string
	cwd      string
}

func statusValues(v *state.View, tv *state.ThreadView, queued int) bandValues {
	ep, _ := endpointOf(v, tv.Key.Endpoint)
	hidden := ep.StatusHide
	model := cmpOr(tv.Model, tv.Thread.Model)
	b := bandValues{model: model, live: tv.Live, cwd: tv.Thread.Cwd, detailOf: map[bandID]string{}}
	b.detailOf[bandModel] = "model " + cmpOr(model, "default")
	add := func(it bandItem, ok bool) {
		if ok && !slices.Contains(hidden, bandNames[it.id]) {
			b.items = append(b.items, it)
			b.detailOf[it.id] = it.detail
		}
	}
	add(effortItem(ep.Models, model, tv.Effort))
	add(bandItem{id: bandApproval, glyph: cmpOr(approvalGlyphs[tv.Approval], tv.Approval), label: "Approval " + tv.Approval, detail: "approval " + tv.Approval}, tv.Approval != "")
	add(sandboxItem(tv.Sandbox), tv.Sandbox != "")
	add(folderItem(tv.Thread.Cwd))
	add(bandItem{id: bandQueue, glyph: glyphQueue, label: queueLabel(queued), count: fmt.Sprint(queued), detail: queueLabel(queued)}, queued > 0)
	add(contextItem(tv.Usage))
	add(bandItem{id: bandMark, label: markDescription, detail: "mark dalgona"}, true)
	return b
}

func cmpOr(values ...string) string {
	for _, s := range values {
		if s != "" {
			return s
		}
	}
	return ""
}

func effortItem(models []rpc.Model, model, effort string) (bandItem, bool) {
	i := slices.IndexFunc(models, func(m rpc.Model) bool { return m.ID == model || m.Model == model })
	if i < 0 {
		return bandItem{}, false
	}
	levels := models[i].SupportedReasoningEfforts
	effort = cmpOr(effort, models[i].DefaultReasoningEffort)
	rank := slices.IndexFunc(levels, func(o rpc.ReasoningEffortOption) bool { return o.ReasoningEffort == effort })
	if len(levels) < 2 || rank < 0 {
		return bandItem{}, false
	}
	glyph := effortGlyphs[int(math.Round(float64(rank*3)/float64(len(levels)-1)))]
	return bandItem{id: bandEffort, glyph: glyph, label: "Effort " + effort, detail: "effort " + effort}, true
}

func sandboxItem(mode string) bandItem {
	glyph := glyphSandbox
	if mode == "off" {
		glyph = glyphNoBox
	}
	return bandItem{id: bandSandbox, glyph: glyph, label: "Sandbox " + mode, detail: "sandbox " + mode}
}

func folderItem(cwd string) (bandItem, bool) {
	parts := strings.FieldsFunc(cwd, func(r rune) bool { return r == '/' || r == '\\' })
	if len(parts) == 0 {
		return bandItem{}, false
	}
	folder := parts[len(parts)-1]
	return bandItem{id: bandFolder, glyph: glyphFolder, label: folder, detail: "folder " + cwd}, true
}

func queueLabel(n int) string {
	if n == 1 {
		return "1 follow-up queued"
	}
	return fmt.Sprintf("%d follow-ups queued", n)
}

func contextFraction(u *rpc.ThreadTokenUsage) (float64, bool) {
	if u == nil || u.ModelContextWindow <= 0 {
		return 0, false
	}
	used := u.Last.InputTokens + u.Last.CachedInputTokens + u.Last.CacheWriteInputTokens + u.Last.OutputTokens
	if u.EstimatedContextTokens != nil {
		used = *u.EstimatedContextTokens
	}
	return min(1, max(0, float64(used)/float64(u.ModelContextWindow))), true
}

func contextItem(u *rpc.ThreadTokenUsage) (bandItem, bool) {
	f, ok := contextFraction(u)
	if !ok {
		return bandItem{}, false
	}
	percent := fmt.Sprintf("%d%%", int(math.Round(f*100)))
	step := int(math.Round(f * moonSteps))
	if step < 0 || step > moonSteps {
		return bandItem{}, false
	}
	glyph := string(rune(moonFirst + step))
	return bandItem{id: bandContext, glyph: glyph, label: "Context " + percent + " used", count: percent, detail: "context " + percent + " used"}, true
}

func (b bandValues) details() []string {
	lines := []string{b.detailOf[bandModel]}
	for _, it := range b.items {
		lines = append(lines, it.detail)
	}
	return lines
}

type band struct {
	model widget.Clickable
	items [bandItemCount]widget.Clickable
}

func (b *band) update(gtx layout.Context, values bandValues, open func(string, *widget.Clickable)) {
	if b.model.Clicked(gtx) && values.live {
		open(modelRoute, &b.model)
	}
	for _, it := range values.items {
		if b.items[it.id].Clicked(gtx) {
			open(statusRoute, &b.items[it.id])
		}
	}
}

func (b *band) layout(gtx layout.Context, shaper *text.Shaper, name theme.Name, values bandValues, compact bool) layout.Dimensions {
	width := gtx.Constraints.Max.X
	var shown []bandItem
	for _, it := range values.items {
		if drop, ok := bandDrops[it.id]; !ok || width > dp(gtx, drop) {
			shown = append(shown, it)
		}
	}
	model := func(gtx layout.Context) layout.Dimensions { return b.modelButton(gtx, shaper, name, values) }
	if compact {
		widgets := []layout.Widget{model}
		for _, it := range shown {
			widgets = append(widgets, b.itemWidget(shaper, name, it))
		}
		return wrapRow(gtx, theme.Space3, widgets...)
	}
	rows := [][]layout.Widget{{model}, nil, nil}
	for _, it := range shown {
		at := 1
		if it.id >= bandContext {
			at = 2
		}
		rows[at] = append(rows[at], b.itemWidget(shaper, name, it))
	}
	return rightRows(gtx, rows)
}

func rightRows(gtx layout.Context, rows [][]layout.Widget) layout.Dimensions {
	width := gtx.Constraints.Max.X
	gap := dp(gtx, theme.Space2)
	y := 0
	for _, items := range rows {
		if len(items) == 0 {
			continue
		}
		if y > 0 {
			y += gap
		}
		lineGtx := gtx
		lineGtx.Constraints.Min = image.Point{}
		call, dims := giodraw.Record(lineGtx, func(gtx layout.Context) layout.Dimensions { return row(gtx, theme.Space3, items...) })
		giodraw.Place(gtx, image.Pt(width-dims.Size.X, y), call)
		y += dims.Size.Y
	}
	return layout.Dimensions{Size: image.Pt(width, y)}
}

func (b *band) modelButton(gtx layout.Context, shaper *text.Shaper, name theme.Name, values bandValues) layout.Dimensions {
	label := cmpOr(values.model, chooseModel)
	return drawButton(gtx, shaper, name.Palette(), button{click: &b.model, label: label, busy: !values.live, note: "Model"})
}

func (b *band) itemWidget(shaper *text.Shaper, name theme.Name, it bandItem) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		p := name.Palette()
		click := &b.items[it.id]
		return click.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			semantic.Button.Add(gtx.Ops)
			semantic.LabelOp(it.label).Add(gtx.Ops)
			semantic.DescriptionOp(bandNames[it.id]).Add(gtx.Ops)
			dims := bandItemContent(gtx, shaper, name, it)
			access.Add(gtx, image.Rectangle{Max: dims.Size}, platform.SemanticNode{ID: platform.NodeID("status-" + bandNames[it.id]), Role: platform.RoleButton, Name: it.label, Description: bandNames[it.id], Action: "activate"}, access.Actions{Click: click})
			if gtx.Focused(click) {
				focusRing(gtx, p.Text, dims.Size, dp(gtx, theme.RadiusTag))
			}
			return dims
		})
	}
}

func bandItemContent(gtx layout.Context, shaper *text.Shaper, name theme.Name, it bandItem) layout.Dimensions {
	p := name.Palette()
	if it.id == bandMark {
		return drawMark(gtx, shaper, name, Mark{Size: markBandSize, Decorative: true})
	}
	icon := textStyle{face: platform.IconFace, weight: theme.WeightText, size: theme.SizeBody, leading: theme.LeadingTight, color: p.Dim, maxLines: 1}
	count := textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingTight, color: p.Dim, maxLines: 1}
	parts := []layout.Widget{func(gtx layout.Context) layout.Dimensions { return paintText(gtx, shaper, icon, it.glyph) }}
	if it.count != "" {
		parts = append(parts, func(gtx layout.Context) layout.Dimensions { return paintText(gtx, shaper, count, it.count) })
	}
	return row(gtx, theme.Space1, parts...)
}
