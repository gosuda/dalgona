//go:build gui

package decision

import (
	"slices"

	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type Dock struct {
	cards     map[state.RequestKey]*card
	recent    []state.RequestKey
	serial    uint64
	queued    []pending
	head      state.RequestKey
	wantFocus bool
	showing   bool
	held      bool
	released  bool
	details   layout.List
	outer     layout.List
	viewport  int
	items     int
}

func NewDock() *Dock {
	return &Dock{
		cards:   map[state.RequestKey]*card{},
		details: layout.List{Axis: layout.Vertical},
		outer:   layout.List{Axis: layout.Vertical},
	}
}

func (d *Dock) Showing() bool { return d.showing }

func (d *Dock) FocusReleased() bool {
	released := d.released
	d.released = false
	return released
}

func (d *Dock) Layout(gtx layout.Context, env shell.Env) layout.Dimensions {
	return d.layoutScene(gtx, env, sceneOf(env.View, env.Thread))
}

func (d *Dock) layoutScene(gtx layout.Context, env shell.Env, sc scene) layout.Dimensions {
	d.announceResolved(env, sc)
	d.queued = sc.queue
	d.prune(sc)
	if len(sc.queue) == 0 {
		d.hide()
		return d.layoutUnsupported(gtx, env, sc.unknown)
	}
	p := sc.queue[0]
	c := d.cardFor(p.view.Key)
	c.refresh(p)
	d.trim(p.view.Key)
	d.lock(c)
	d.arrive(env, p, c)
	ps := paints{shaper: env.Shaper, palette: env.Theme.Palette(), point: env.Theme.Point(env.Thread.Thread)}
	c.updateAnswers(gtx, env, p)
	acts := c.actions(p)
	d.handle(gtx, env, p, c, acts)
	f := frame{pending: p, card: c, actions: c.actions(p), total: len(sc.queue), unknown: sc.unknown, focus: !env.Modal, warning: c.warningNote(p)}
	dims := d.semanticCard(gtx, ps, f)
	d.showing, d.held = true, c.holds(gtx)
	return dims
}

func (d *Dock) semanticCard(gtx layout.Context, ps paints, f frame) layout.Dimensions {
	k := f.pending.view.Key
	id := access.Path("decision", string(k.Thread.Endpoint), k.Thread.Thread, k.Request)
	n := platform.SemanticNode{ID: platform.NodeID(id), Role: platform.RoleDialog, Name: f.card.details.title()}
	return access.Group(gtx, n, access.Actions{}, func(gtx layout.Context) layout.Dimensions { return d.layoutCard(gtx, ps, f) })
}

func (d *Dock) announceResolved(env shell.Env, sc scene) {
	for _, p := range d.queued {
		if sc.queued(p.view.Key) {
			continue
		}
		if r, ok := sc.resolution(p.view.Key); ok {
			env.Announce(platform.Announcement{Kind: platform.AnnounceCompletion, Text: resolutionText(r, p.endpoint)})
		}
	}
}

func (d *Dock) prune(sc scene) {
	for key := range d.cards {
		if sc.queued(key) {
			continue
		}
		if _, resolved := sc.resolution(key); resolved || sc.covered[key.Thread] {
			d.drop(key)
		}
	}
}

func (d *Dock) drop(key state.RequestKey) {
	delete(d.cards, key)
	d.recent = slices.DeleteFunc(d.recent, func(k state.RequestKey) bool { return k == key })
}

func (d *Dock) cardFor(key state.RequestKey) *card {
	d.recent = slices.DeleteFunc(d.recent, func(k state.RequestKey) bool { return k == key })
	d.recent = append(d.recent, key)
	c, ok := d.cards[key]
	if !ok {
		d.serial++
		c = &card{key: key, serial: d.serial}
		d.cards[key] = c
	}
	return c
}

func (d *Dock) hide() {
	if d.showing && d.held {
		d.released = true
	}
	d.showing, d.held, d.head, d.wantFocus = false, false, state.RequestKey{}, false
}

func (d *Dock) arrive(env shell.Env, p pending, c *card) {
	if p.view.Key != d.head {
		d.head, d.wantFocus = p.view.Key, true
		d.details.Position, d.outer.Position = layout.Position{}, layout.Position{}
	}
	if !c.announced {
		c.announced = true
		env.Announce(platform.Announcement{Kind: platform.AnnounceDecision, Text: c.details.kicker() + " on " + p.endpoint})
	}
}

func (d *Dock) handle(gtx layout.Context, env shell.Env, p pending, c *card, acts []action) {
	if c.retry.Clicked(gtx) && p.view.Err != nil {
		env.Submit(state.RetryDetails{Request: p.view.Key})
	}
	for i, a := range acts {
		if c.clicks[i].Clicked(gtx) && a.reason == "" {
			d.act(env, p, c, a)
		}
	}
	d.fieldActions(env, p, c, acts)
	d.keys(gtx, env, p, c, acts)
}

func (d *Dock) fieldActions(env shell.Env, p pending, c *card, acts []action) {
	for _, t := range c.triggers {
		for _, a := range acts {
			if a.trigger == t && a.reason == "" {
				d.act(env, p, c, a)
			}
		}
	}
}

func (d *Dock) keys(gtx layout.Context, env shell.Env, p pending, c *card, acts []action) {
	filters := c.keyFilters(acts)
	for {
		ev, ok := gtx.Event(filters...)
		if !ok {
			return
		}
		if e, isKey := ev.(key.Event); isKey && e.State == key.Press {
			d.press(gtx, env, p, c, e.Name)
		}
	}
}

func (d *Dock) act(env shell.Env, p pending, c *card, a action) {
	if !c.open(p) {
		return
	}
	env.Submit(state.Answer{Request: p.view.Key, Result: a.reply(c)})
	c.sent, c.sentWarning, c.sentLink = true, p.view.Warning, p.link
}

func (d *Dock) press(gtx layout.Context, env shell.Env, p pending, c *card, name key.Name) {
	if d.scroll(gtx, name) {
		return
	}
	for _, a := range c.actions(p) {
		if a.key.name == name && a.reason == "" {
			d.act(env, p, c, a)
			return
		}
	}
}

var scrollKeys = [...]key.Name{key.NameUpArrow, key.NameDownArrow, key.NamePageUp, key.NamePageDown, key.NameHome, key.NameEnd}

type scrollStep struct {
	lines, pages int
}

var scrollSteps = map[key.Name]scrollStep{
	key.NameUpArrow: {lines: -1}, key.NameDownArrow: {lines: 1}, key.NamePageUp: {pages: -1}, key.NamePageDown: {pages: 1},
}

func (d *Dock) scroll(gtx layout.Context, name key.Name) bool {
	if name == key.NameHome {
		d.details.Position = layout.Position{}
		return true
	}
	if name == key.NameEnd {
		d.details.Position = layout.Position{First: d.items}
		return true
	}
	s, ok := scrollSteps[name]
	if !ok {
		return false
	}
	line := dp(gtx, theme.RequestScrollStep)
	d.details.Position.Offset += s.lines*line + s.pages*max(line, d.viewport-line)
	return true
}

func (c *card) keyFilters(acts []action) []event.Filter {
	owners := c.owners(len(acts))
	names := append([]key.Name(nil), scrollKeys[:]...)
	for _, a := range acts {
		if a.key.name != "" {
			names = append(names, a.key.name)
		}
	}
	filters := make([]event.Filter, 0, len(owners)*len(names)+len(c.answers))
	for _, o := range owners {
		for _, n := range names {
			filters = append(filters, key.Filter{Focus: o, Name: n})
		}
	}
	for _, tag := range c.fields() {
		filters = append(filters, key.Filter{Focus: tag, Name: key.NameEscape})
	}
	return filters
}

func (c *card) owners(actions int) []event.Tag {
	owners := []event.Tag{&c.title, &c.retry}
	for i := range actions {
		owners = append(owners, &c.clicks[i])
	}
	for i := range c.answers {
		a := &c.answers[i]
		owners = append(owners, &a.copy)
		for j := range a.picks {
			owners = append(owners, &a.picks[j])
		}
	}
	return owners
}

func (c *card) fields() []event.Tag {
	tags := make([]event.Tag, 0, len(c.answers))
	for i := range c.answers {
		if c.answers[i].field != nil {
			tags = append(tags, c.answers[i].field.Tag())
		}
	}
	return tags
}

func (c *card) holds(gtx layout.Context) bool {
	return slices.ContainsFunc(c.owners(len(c.clicks)), gtx.Focused) || slices.ContainsFunc(c.fields(), gtx.Focused)
}
