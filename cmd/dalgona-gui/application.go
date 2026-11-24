//go:build gui

package main

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"log/slog"
	"maps"
	"slices"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/manage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	windowPrefix   = "window-"
	appearanceWait = 2 * time.Second
	settingTheme   = "theme"
)

var errNoWindow = errors.New("no dalgona window is open; open a window and try again")

type notifiedSink interface {
	Notified(link client.Link, method string, params jsontext.Value)
}

type observer struct {
	*state.Owner
	sink atomic.Pointer[notifiedSink]
}

func (o *observer) forward(s notifiedSink) { o.sink.Store(&s) }

func (o *observer) Notified(link client.Link, method string, params jsontext.Value) {
	o.Owner.Notified(link, method, params)
	if s := o.sink.Load(); s != nil {
		(*s).Notified(link, method, params)
	}
}

type look struct {
	dark     bool
	contrast bool
	still    bool
}

type presence struct {
	focused bool
	open    state.ThreadKey
}

type application struct {
	ctx       context.Context
	dir       string
	figures   *figure.Hub
	owner     *state.Owner
	observer  *observer
	hub       *manage.Hub
	notifier  *platform.Notifier
	attention *shell.Attention
	theme     *preference
	ready     chan struct{}
	readyOnce sync.Once
	looks     atomic.Pointer[look]
	reread    chan struct{}
	windows   sync.WaitGroup

	mu       sync.Mutex
	open     map[state.WindowID]*window
	presence map[state.WindowID]presence
	recent   state.WindowID
	failure  error
	lookErr  string
}

func newApplication(ctx context.Context, dir string, figures *figure.Supervisor) *application {
	a := &application{
		ctx: ctx, dir: dir, ready: make(chan struct{}), reread: make(chan struct{}, 1),
		open: map[state.WindowID]*window{}, presence: map[state.WindowID]presence{}, theme: loadPreference(dir, themeFile),
	}
	a.notifier = platform.NewNotifier(a.invalidate)
	a.figures = figure.NewHub(figures, a.invalidate)
	a.owner = state.New(a.published, dir)
	a.observer = &observer{Owner: a.owner}
	notifications := loadPreference(dir, preferenceFile)
	a.attention = shell.NewAttention(a.notifier, notifications.get() == "on", func(on bool) { notifications.set(onOff(on)) }, a.invalidate)
	a.looks.Store(&look{})
	return a
}

func (a *application) attachHub(h *manage.Hub) {
	a.hub = h
	a.observer.forward(h)
}

func (a *application) runOwner(ctx context.Context, set *client.EndpointSet, logger *slog.Logger) <-chan struct{} {
	stopped := make(chan struct{})
	go func() {
		defer close(stopped)
		if err := a.owner.Run(ctx, set); err != nil && ctx.Err() == nil {
			logger.Error("state owner stopped", "error", err)
		}
	}()
	return stopped
}

func (a *application) published() {
	a.readyOnce.Do(func() { close(a.ready) })
	a.announce()
	a.invalidate()
}

func (a *application) announce() {
	a.announceView(a.view())
}

func (a *application) announceView(v *state.View) {
	a.attention.Observe(v, a.visible)
}

func (a *application) invalidate() {
	a.mu.Lock()
	defer a.mu.Unlock()
	for _, win := range a.open {
		win.w.Invalidate()
	}
}

func (a *application) serve(ctx context.Context, stopped <-chan struct{}) error {
	a.readAppearance(ctx)
	go a.followAppearance(ctx)
	go a.attention.Run(ctx)
	go a.routeActivations(ctx)
	a.attention.Refresh()
	select {
	case <-a.ready:
	case <-stopped:
		return errors.New("the GUI state owner stopped before it loaded the saved windows")
	}
	if err := a.restore(); err != nil {
		return err
	}
	a.windows.Wait()
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.failure
}

func (a *application) view() *state.View {
	if v := a.owner.View(); v != nil {
		return v
	}
	return state.Snapshot{}.View()
}

func (a *application) restore() error {
	saved := a.view().Windows()
	if len(saved) == 0 {
		return a.openWindow(a.freeWindowID(), state.ThreadKey{})
	}
	for _, wv := range saved {
		if err := a.openWindow(wv.ID, a.adopt(wv)); err != nil {
			return err
		}
	}
	return nil
}

func (a *application) adopt(wv state.WindowView) state.ThreadKey {
	last := len(wv.Threads) - 1
	for _, key := range wv.Threads[:last] {
		a.owner.Submit(state.CloseView{Window: wv.ID, Thread: key})
	}
	return wv.Threads[last]
}

func (a *application) openWindow(id state.WindowID, thread state.ThreadKey) error {
	win, err := newWindow(a, id)
	if err != nil {
		return err
	}
	if thread.Thread != "" {
		win.frame.Adopt(thread)
	}
	a.mu.Lock()
	a.open[id] = win
	a.mu.Unlock()
	a.windows.Add(1)
	go a.runWindow(win)
	return nil
}

func (a *application) newWindow() {
	if err := a.openWindow(a.freeWindowID(), state.ThreadKey{}); err != nil {
		slog.Error("open a new window", "error", err)
	}
}

func (a *application) runWindow(win *window) {
	defer a.windows.Done()
	a.closed(win, win.loop())
}

func (a *application) closed(win *window, err error) {
	win.cancel()
	a.mu.Lock()
	delete(a.open, win.id)
	delete(a.presence, win.id)
	final := len(a.open) == 0
	if final {
		a.failure = err
	}
	a.mu.Unlock()
	if !final {
		a.owner.Submit(state.CloseWindow{Window: win.id})
	}
	if err != nil && !final {
		slog.Error("a window closed with an error", "window", win.id, "error", err)
	}
}

func (a *application) freeWindowID() state.WindowID {
	saved := a.view().Windows()
	a.mu.Lock()
	defer a.mu.Unlock()
	for n := 1; ; n++ {
		id := state.WindowID(windowPrefix + strconv.Itoa(n))
		_, used := a.open[id]
		if !used && !slices.ContainsFunc(saved, func(wv state.WindowView) bool { return wv.ID == id }) {
			return id
		}
	}
}

func (a *application) closeScope(id state.WindowID) shell.CloseScope {
	a.mu.Lock()
	win := a.open[id]
	final := len(a.open) <= 1
	a.mu.Unlock()
	v := a.view()
	scope := shell.CloseScope{Final: final}
	if !final {
		scope.Elsewhere = func(k state.ThreadKey) bool { return watchedElsewhere(v, id, k) }
	}
	if win != nil && win.terminal != nil {
		if terminalID, thread, ok := win.terminal.AttachedSession(); ok {
			name := thread.Thread
			if tv, found := v.Thread(thread); found && tv.Thread.Name != "" {
				name = tv.Thread.Name
			}
			scope.Terminals = []string{"Terminal " + terminalID + " in " + name}
		}
	}
	return scope
}

func watchedElsewhere(v *state.View, id state.WindowID, k state.ThreadKey) bool {
	return slices.ContainsFunc(v.Windows(), func(wv state.WindowView) bool {
		return wv.ID != id && slices.Contains(wv.Threads, k)
	})
}

func (a *application) observe(win *window) {
	a.mu.Lock()
	a.presence[win.id] = presence{focused: win.focused, open: win.frame.Open()}
	if win.focused {
		a.recent = win.id
	}
	a.mu.Unlock()
}

func (a *application) visible(k state.ThreadKey) bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	for _, p := range a.presence {
		if p.focused && p.open == k {
			return true
		}
	}
	return false
}

func (a *application) routeActivations(ctx context.Context) {
	for {
		select {
		case <-ctx.Done():
			return
		case tag := <-a.notifier.Activations():
			if key, ok := shell.ParseTag(tag); ok {
				a.activate(key)
			}
		}
	}
}

func (a *application) activate(key state.ThreadKey) {
	a.mu.Lock()
	target := a.open[a.windowFor(key)]
	a.mu.Unlock()
	if target != nil {
		target.activate(key)
	}
}

func (a *application) windowFor(key state.ThreadKey) state.WindowID {
	ids := slices.Sorted(maps.Keys(a.open))
	if i := slices.IndexFunc(ids, func(id state.WindowID) bool { return a.presence[id].open == key }); i >= 0 {
		return ids[i]
	}
	return a.recentWindow(ids)
}

func (a *application) recentWindow(ids []state.WindowID) state.WindowID {
	if _, ok := a.open[a.recent]; ok || len(ids) == 0 {
		return a.recent
	}
	return ids[0]
}

func (a *application) focusedWindow() *window {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.open[a.recentWindow(slices.Sorted(maps.Keys(a.open)))]
}

func (a *application) choose(ctx context.Context, req platform.DialogRequest) (platform.DialogResult, error) {
	win := a.focusedWindow()
	if win == nil {
		return platform.DialogResult{}, errNoWindow
	}
	return win.dialogs.choose(ctx, req)
}

func (a *application) browse(url string) error { return platform.OpenLink(a.ctx, url) }

func (a *application) look() look { return *a.looks.Load() }

func (a *application) readAppearance(ctx context.Context) {
	ctx, cancel := context.WithTimeout(ctx, appearanceWait)
	defer cancel()
	var next look
	var darkErr, contrastErr, motionErr error
	next.dark, darkErr = platform.PrefersDark(ctx)
	next.contrast, contrastErr = platform.HighContrast(ctx)
	next.still, motionErr = platform.ReducedMotion(ctx)
	a.looks.Store(&next)
	a.reportAppearance(errors.Join(darkErr, contrastErr, motionErr))
}

func (a *application) reportAppearance(err error) {
	text := ""
	if err != nil {
		text = err.Error()
	}
	a.mu.Lock()
	changed := text != a.lookErr
	a.lookErr = text
	a.mu.Unlock()
	if changed && err != nil {
		slog.Warn("read the desktop appearance; dalgona uses the day theme, ordinary contrast, and motion until it can", "error", err)
	}
}

func (a *application) followAppearance(ctx context.Context) {
	go func() {
		if err := platform.WatchAppearance(ctx, a.appearanceChanged); err != nil {
			slog.Warn("follow desktop appearance changes; dalgona rereads them when a window gains focus", "error", err)
		}
	}()
	for {
		select {
		case <-ctx.Done():
			return
		case <-a.reread:
			a.readAppearance(ctx)
			a.invalidate()
		}
	}
}

func (a *application) appearanceChanged() {
	select {
	case a.reread <- struct{}{}:
	default:
	}
}

func (a *application) themeFor(v *state.View, win *window, l look) theme.Name {
	setting, ok := daemonTheme(v, win.frame.Open().Endpoint)
	if ok {
		if a.isRecent(win.id) {
			a.theme.set(string(setting))
		}
	} else {
		setting = theme.ParseSetting(a.theme.get())
	}
	name := setting.Resolve(l.dark)
	if l.contrast {
		name = name.WithHighContrast()
	}
	return name
}

func (a *application) isRecent(id state.WindowID) bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.recent == id
}

func daemonTheme(v *state.View, preferred client.EndpointID) (theme.Setting, bool) {
	endpoints := v.Endpoints()
	if i := slices.IndexFunc(endpoints, func(ep state.EndpointView) bool { return ep.ID == preferred }); i >= 0 {
		if s, ok := settingOf(endpoints[i]); ok {
			return s, true
		}
	}
	for _, ep := range endpoints {
		if s, ok := settingOf(ep); ok {
			return s, true
		}
	}
	return "", false
}

func settingOf(ep state.EndpointView) (theme.Setting, bool) {
	i := slices.IndexFunc(ep.Settings, func(s rpc.Setting) bool { return s.Key == settingTheme })
	if i < 0 {
		return "", false
	}
	word, ok := ep.Settings[i].Value.(string)
	return theme.ParseSetting(word), ok
}
