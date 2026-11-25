//go:build gui

package composer

import (
	"cmp"
	"slices"
)

type prerequisite uint8

const (
	needsThread prerequisite = 1 << iota
	needsIdle
)

type command struct {
	name    string
	summary string
	needs   prerequisite
}

var commands = [...]command{
	{name: "model", summary: "Choose a model or preset"},
	{name: "settings", summary: "Change theme, verbosity, approval, status items, edit review, and notifications"},
	{name: "login", summary: "Log in to a provider"},
	{name: "resume", summary: "Open an earlier thread"},
	{name: "new", summary: "Start a new thread", needs: needsIdle},
	{name: "compact", summary: "Compact this thread", needs: needsThread | needsIdle},
	{name: "fork", summary: "Fork this thread at a turn", needs: needsThread | needsIdle},
	{name: "tree", summary: "Move to another branch of this thread", needs: needsThread | needsIdle},
	{name: "revert", summary: "Rewind to before a turn", needs: needsThread | needsIdle},
	{name: "advisor", summary: "Show or change the advisor", needs: needsThread},
	{name: "review", summary: "Review changes", needs: needsThread},
	{name: "help", summary: "Show keys"},
	{name: "remote", summary: "Manage daemon endpoints"},
	{name: "runner", summary: "Inspect the runner and transfer files", needs: needsThread},
	{name: "mcp", summary: "Configure and authenticate MCP servers"},
	{name: "files", summary: "Browse and edit workspace files", needs: needsThread},
	{name: "terminal", summary: "Open or attach a terminal", needs: needsThread},
	{name: "doctor", summary: "Run diagnostics"},
	{name: "failures", summary: "Inspect failure records and reporting consent"},
}

const (
	reasonNeedsThread = "Open a thread first"
	reasonNeedsIdle   = "Wait for the turn to finish or interrupt it first"
)

type threadContext struct {
	open    bool
	running bool
}

func (c command) unmet(ctx threadContext) string {
	switch {
	case c.needs&needsThread != 0 && !ctx.open:
		return reasonNeedsThread
	case c.needs&needsIdle != 0 && ctx.running:
		return reasonNeedsIdle
	}
	return ""
}

func lookupCommand(name string) (command, bool) {
	at := slices.IndexFunc(commands[:], func(c command) bool { return c.name == name })
	if at < 0 {
		return command{}, false
	}
	return commands[at], true
}

type ranked struct {
	command command
	prefix  bool
	spread  int
	order   int
}

func rankCommands(query string) []command {
	q := asciiLower(query)
	found := make([]ranked, 0, len(commands))
	for i, c := range commands {
		if r, ok := rankCommand(c, q); ok {
			r.order = i
			found = append(found, r)
		}
	}
	slices.SortStableFunc(found, func(a, b ranked) int {
		if a.prefix != b.prefix {
			if a.prefix {
				return -1
			}
			return 1
		}
		return cmp.Or(cmp.Compare(a.spread, b.spread), cmp.Compare(a.order, b.order))
	})
	out := make([]command, len(found))
	for i, r := range found {
		out[i] = r.command
	}
	return out
}

func rankCommand(c command, q string) (ranked, bool) {
	if len(q) <= len(c.name) && c.name[:len(q)] == q {
		return ranked{command: c, prefix: true}, true
	}
	spread, ok := subsequenceSpread(c.name, q)
	return ranked{command: c, spread: spread}, ok
}

func subsequenceSpread(name, q string) (int, bool) {
	first, last, at := -1, -1, 0
	for i := 0; i < len(name) && at < len(q); i++ {
		if name[i] != q[at] {
			continue
		}
		if first < 0 {
			first = i
		}
		last = i
		at++
	}
	if at < len(q) {
		return 0, false
	}
	return last - first + 1, true
}

func asciiLower(s string) string {
	b := []byte(s)
	for i, c := range b {
		if 'A' <= c && c <= 'Z' {
			b[i] = c + 'a' - 'A'
		}
	}
	return string(b)
}
