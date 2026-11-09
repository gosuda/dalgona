package event

type Seqd struct {
	Seq   uint64
	Event Event
}

type TurnNode struct {
	TurnID       string
	ParentTurnID string
	Preview      string
	Active       bool
}

func ActivePath(events []Seqd) []Seqd {
	positions := make(map[uint64]int, len(events))
	for i, item := range events {
		positions[item.Seq] = i
	}
	included := make([]bool, len(events))
	cursor := len(events) - 1
	for cursor >= 0 {
		branch, ok := events[cursor].Event.(Branch)
		if !ok {
			included[cursor] = true
			cursor--
			continue
		}
		if branch.ParentSeq == 0 {
			break
		}
		parent, exists := positions[branch.ParentSeq]
		if !exists || parent >= cursor {
			break
		}
		cursor = parent
	}
	path := make([]Seqd, 0, len(events))
	for i, item := range events {
		if included[i] {
			path = append(path, item)
		}
	}
	return path
}

func Tree(events []Seqd) []TurnNode {
	projection := treeProjection{
		indexes: make(map[string]int), completed: make(map[uint64]string), previews: make(map[string]bool),
	}
	for _, item := range events {
		projection.add(item)
	}
	active := activeTurns(events)
	for i := range projection.nodes {
		_, projection.nodes[i].Active = active[projection.nodes[i].TurnID]
	}
	return projection.nodes
}

type treeProjection struct {
	nodes     []TurnNode
	indexes   map[string]int
	completed map[uint64]string
	previews  map[string]bool
	leaf      string
}

func (p *treeProjection) add(item Seqd) {
	switch value := item.Event.(type) {
	case TurnStarted:
		p.indexes[value.TurnID] = len(p.nodes)
		p.nodes = append(p.nodes, TurnNode{TurnID: value.TurnID, ParentTurnID: p.leaf})
		p.leaf = value.TurnID
	case TurnCompleted:
		p.completed[item.Seq] = value.TurnID
		p.leaf = value.TurnID
	case UserMessage:
		p.setPreview(value)
	case Branch:
		p.leaf = p.completed[value.ParentSeq]
	}
}

func (p *treeProjection) setPreview(message UserMessage) {
	index, exists := p.indexes[message.TurnID]
	if !exists || p.previews[message.TurnID] || message.Kind != "prompt" {
		return
	}
	text := []rune(message.Text)
	p.nodes[index].Preview = string(text[:min(len(text), 80)])
	p.previews[message.TurnID] = true
}

func activeTurns(events []Seqd) map[string]struct{} {
	active := make(map[string]struct{})
	for _, item := range ActivePath(events) {
		turn, ok := item.Event.(TurnStarted)
		if ok {
			active[turn.TurnID] = struct{}{}
		}
	}
	return active
}
