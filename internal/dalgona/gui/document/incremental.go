package document

import "sort"

func (d *Document) appendMarkdown(previous *Document) int {
	tail := previous.tailRegion()
	start, first := 0, 0
	if tail < len(previous.regions) {
		start, first = previous.regions[tail].start, previous.regions[tail].first
	}
	kept := sort.Search(len(previous.definitions), func(i int) bool { return previous.definitions[i].start >= start })
	p := parseMarkdown(d.source, start, len(d.source), winners(previous.definitions[:kept]))

	d.regions = make([]region, tail, tail+len(p.regions))
	copy(d.regions, previous.regions[:tail])
	for _, r := range p.regions {
		r.first += first
		d.regions = append(d.regions, r)
	}
	d.blocks = make([]Block, first, first+len(p.blocks))
	copy(d.blocks, previous.blocks[:first])
	d.blocks = append(d.blocks, p.blocks...)
	d.definitions = make([]definition, kept, kept+len(p.definitions))
	copy(d.definitions, previous.definitions[:kept])
	d.definitions = append(d.definitions, p.definitions...)

	from := min(first, d.invalidate(previous.definitions, tail))
	d.finish(from)
	return from
}

func (d *Document) tailRegion() int {
	if len(d.regions) == 0 {
		return 0
	}
	tail := len(d.regions) - 1
	for tail > 0 && d.regions[tail-1].definition {
		tail--
	}
	return tail
}

func winners(definitions []definition) []definition {
	seen := make(map[string]bool, len(definitions))
	out := make([]definition, 0, len(definitions))
	for _, d := range definitions {
		if !seen[d.label] {
			seen[d.label] = true
			out = append(out, d)
		}
	}
	return out
}

type target struct {
	destination string
	title       string
}

func changedLabels(before, after []definition) map[string]bool {
	targets := make(map[string]target, len(before))
	for _, d := range winners(before) {
		targets[d.label] = target{destination: d.destination, title: d.title}
	}
	changed := map[string]bool{}
	for _, d := range winners(after) {
		if t, ok := targets[d.label]; !ok || t != (target{destination: d.destination, title: d.title}) {
			changed[d.label] = true
		}
		delete(targets, d.label)
	}
	for label := range targets {
		changed[label] = true
	}
	return changed
}

func (d *Document) invalidate(before []definition, limit int) int {
	changed := changedLabels(before, d.definitions)
	earliest := len(d.blocks)
	if len(changed) == 0 {
		return earliest
	}
	resolved := winners(d.definitions)
	for r := range limit {
		if !mentions(d.regions[r].labels, changed) {
			continue
		}
		d.reparse(r, resolved)
		earliest = min(earliest, d.regions[r].first)
	}
	return earliest
}

func mentions(labels []string, changed map[string]bool) bool {
	for _, label := range labels {
		if changed[label] {
			return true
		}
	}
	return false
}

func (d *Document) reparse(r int, definitions []definition) {
	end, next := len(d.source), len(d.blocks)
	if r+1 < len(d.regions) {
		end, next = d.regions[r+1].start, d.regions[r+1].first
	}
	p := parseMarkdown(d.source, d.regions[r].start, end, definitions)
	first := d.regions[r].first
	blocks := make([]Block, 0, len(d.blocks)-(next-first)+len(p.blocks))
	blocks = append(blocks, d.blocks[:first]...)
	blocks = append(blocks, p.blocks...)
	blocks = append(blocks, d.blocks[next:]...)
	d.blocks = blocks
	var diagnostics []Diagnostic
	for _, parsedRegion := range p.regions {
		diagnostics = append(diagnostics, parsedRegion.diagnostics...)
	}
	d.regions[r].diagnostics = diagnostics
	shift := len(p.blocks) - (next - first)
	for i := r + 1; i < len(d.regions); i++ {
		d.regions[i].first += shift
	}
}
