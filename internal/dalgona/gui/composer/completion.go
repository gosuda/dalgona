//go:build gui

package composer

import (
	"fmt"
	"path"
	"strings"
	"time"

	"gioui.org/layout"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const fileRowLimit = 20

type fileRequest struct {
	key     state.DraftKey
	cwd     string
	query   string
	span    textinput.Range
	textGen uint64
	ticket  uint64
}

type completion struct {
	request fileRequest
	due     time.Time
	active  int
	closed  bool
	shown   []string
	rows    choiceList
}

func (c *completion) reset() {
	c.request, c.due, c.active, c.closed, c.shown = fileRequest{}, time.Time{}, 0, false, nil
}

func (c *completion) dismiss() { c.closed, c.shown = true, nil }

func (c *completion) requested() bool { return c.request.query != "" && !c.closed }

func (d *Dock) mention(env frame) (mention, string, bool) {
	tv, ok := threadOf(env)
	if !ok || tv.Thread.Cwd == "" || d.editor.Composing() {
		return mention{}, "", false
	}
	current := d.editor.Draft()
	if parseEntry(current.Text).kind == entryCommand || current.Selection.Start != current.Selection.End {
		return mention{}, "", false
	}
	m, found := mentionAt(current.Text, current.Selection.End)
	return m, tv.Thread.Cwd, found && m.query != ""
}

func (c *completion) advance(gtx layout.Context, d *Dock, env frame) {
	m, cwd, ok := d.mention(env)
	if !ok {
		c.reset()
		return
	}
	next := fileRequest{key: d.key, cwd: cwd, query: m.query, span: m.span, textGen: d.draft.textGen}
	if next != (fileRequest{key: c.request.key, cwd: c.request.cwd, query: c.request.query, span: c.request.span, textGen: c.request.textGen}) {
		c.request, c.active, c.closed, c.shown = next, 0, false, nil
		c.due = gtx.Now.Add(settleDelay)
	}
	c.submit(gtx, d, env)
	c.shown = c.paths(env)
}

func (c *completion) submit(gtx layout.Context, d *Dock, env frame) {
	if c.request.ticket != 0 {
		return
	}
	if gtx.Now.Before(c.due) {
		gtx.Execute(op.InvalidateCmd{At: c.due})
		return
	}
	c.request.ticket = d.nextTicket()
	env.Submit(state.SearchFiles{Key: c.request.key, Query: c.request.query, Ticket: c.request.ticket})
}

func (c *completion) result(env frame) (state.FileSearch, bool) {
	if c.request.ticket == 0 || c.closed {
		return state.FileSearch{}, false
	}
	fs := env.view.FileSearch(c.request.key)
	return fs, fs.Ticket == c.request.ticket && fs.Query == c.request.query
}

func (c *completion) paths(env frame) []string {
	fs, ok := c.result(env)
	if !ok || fs.Phase != state.SearchDone {
		return nil
	}
	return fs.Paths[:min(len(fs.Paths), fileRowLimit)]
}

func (c *completion) open(d *Dock) bool {
	return !d.palette.open(d) && len(c.shown) > 0
}

func (c *completion) pending(env frame) bool {
	if !c.requested() || len(c.shown) > 0 {
		return false
	}
	fs, ok := c.result(env)
	return !ok || fs.Phase == state.SearchAuthorizing || fs.Phase == state.SearchSearching
}

func (c *completion) move(step int) {
	count := len(c.shown)
	if count == 0 {
		return
	}
	c.active = (min(c.active, count-1) + step + count) % count
}

func (c *completion) insert(d *Dock) {
	if len(c.shown) == 0 {
		return
	}
	chosen := c.shown[min(c.active, len(c.shown)-1)]
	current := d.editor.Draft()
	m, ok := mentionAt(current.Text, current.Selection.End)
	bound := ok && !d.editor.Composing() && c.request.key == d.key && c.request.textGen == d.draft.textGen && m.span == c.request.span
	if !bound {
		d.notice.fail("The text changed before the file was chosen; choose it again.")
		return
	}
	if err := d.editor.Replace(c.request.span, insertedPath(chosen)); err != nil {
		d.notice.reject(rejectionText(err), d.editor.TakeRejected())
		return
	}
	c.reset()
}

func fileRows(paths []string) []listRow {
	rows := make([]listRow, len(paths))
	for i, p := range paths {
		rows[i] = listRow{primary: path.Base(strings.ReplaceAll(p, `\`, "/")), secondary: p}
	}
	return rows
}

func (c *completion) status(env frame) string {
	if !c.requested() {
		return ""
	}
	fs, ok := c.result(env)
	if !ok {
		return "Searching files…"
	}
	return searchStatus(fs, c.request.query)
}

func searchStatus(fs state.FileSearch, query string) string {
	switch fs.Phase {
	case state.SearchAuthorizing:
		return "Waiting for read access to the workspace"
	case state.SearchDeclined:
		return "Read access to the workspace was declined; file completion is off for this thread"
	case state.SearchNoDirectory:
		return "This thread has no known directory; file completion is off"
	case state.SearchSearching:
		return "Searching files…"
	case state.SearchFailed:
		return fmt.Sprintf("File search failed: %v", fs.Err)
	case state.SearchDone:
	}
	return resultStatus(fs, query)
}

func resultStatus(fs state.FileSearch, query string) string {
	switch {
	case len(fs.Paths) == 0 && fs.Incomplete:
		return fmt.Sprintf("No files match @%s so far; the search stopped before checking every file", query)
	case len(fs.Paths) == 0:
		return fmt.Sprintf("No files match @%s in %s", query, fs.Root)
	case fs.Incomplete:
		return "Partial results; the search stopped before checking every file"
	}
	return ""
}
