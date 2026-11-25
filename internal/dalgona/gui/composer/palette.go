//go:build gui

package composer

type palette struct {
	closed bool
	query  string
	active int
	rows   choiceList
}

func (p *palette) reopen() { p.closed = false }

func (p *palette) current(d *Dock) (string, bool) {
	if p.closed {
		return "", false
	}
	return parseEntry(d.editor.Draft().Text).commandQuery()
}

func (p *palette) matches(d *Dock) []command {
	query, ok := p.current(d)
	if !ok {
		return nil
	}
	if query != p.query {
		p.query, p.active = query, 0
	}
	return rankCommands(query)
}

func (p *palette) open(d *Dock) bool {
	_, ok := p.current(d)
	return ok
}

func (p *palette) move(step int) {
	count := len(rankCommands(p.query))
	if count == 0 {
		return
	}
	p.active = (min(p.active, count-1) + step + count) % count
}

func (d *Dock) openSelected(env frame) {
	matches := d.palette.matches(d)
	if len(matches) == 0 {
		query, _ := d.palette.current(d)
		d.notice.fail(unknownCommand(query))
		return
	}
	d.openCommand(env, matches[min(d.palette.active, len(matches)-1)])
}

func (d *Dock) paletteRows(env frame) []listRow {
	matches := d.palette.matches(d)
	ctx := d.context(env)
	rows := make([]listRow, len(matches))
	for i, c := range matches {
		rows[i] = listRow{primary: "/" + c.name, secondary: c.summary, reason: c.unmet(ctx)}
	}
	return rows
}
