package textinput

import "unicode/utf8"

type emojiTail uint8

const (
	emojiNone emojiTail = iota
	emojiPictographic
	emojiJoined
)

type cluster struct {
	last               property
	conjunctLinked     bool
	emoji              emojiTail
	regionalIndicators int
}

func Boundaries(s string) []int {
	return segmentBoundaries(s, clusterEnd)
}

func NextClusterBoundary(s string, i int) (int, error) {
	if !isBoundary(s, i) {
		return 0, InvalidOffset
	}
	if i == len(s) {
		return i, nil
	}
	return clusterEnd(s, i), nil
}

func PreviousClusterBoundary(s string, i int) (int, error) {
	if !isBoundary(s, i) {
		return 0, InvalidOffset
	}
	if i == 0 {
		return 0, nil
	}
	start, _ := clusterAt(s, i-1)
	return start, nil
}

func isBoundary(s string, i int) bool {
	if i < 0 || i > len(s) {
		return false
	}
	if i == len(s) {
		return true
	}
	start, _ := clusterAt(s, i)
	return start == i
}

func boundaryAtOrAfter(s string, i int) int {
	if i == len(s) {
		return i
	}
	start, end := clusterAt(s, i)
	if start == i {
		return i
	}
	return end
}

func clusterAt(s string, i int) (start, end int) {
	start = guaranteedBoundary(s, i)
	for end = clusterEnd(s, start); end <= i; end = clusterEnd(s, start) {
		start = end
	}
	return start, end
}

func guaranteedBoundary(s string, i int) int {
	for i > 0 && !utf8.RuneStart(s[i]) {
		i--
	}
	return safeStart(s, i, clusterBreaksAt)
}

func clusterBreaksAt(s string, i int) bool {
	r, _ := utf8.DecodeRuneInString(s[i:])
	before, _ := utf8.DecodeLastRuneInString(s[:i])
	next := lookup(r).property
	return startsWithoutContext(next) && !joinsPair(lookup(before).breakClass(), next.breakClass())
}

func startsWithoutContext(p property) bool {
	return !p.pictographic() && p.conjunct() != incbConsonant && p.breakClass() != regionalIndicator
}

func clusterEnd(s string, start int) int {
	r, size := utf8.DecodeRuneInString(s[start:])
	var c cluster
	c.add(lookup(r).property)
	end := start + size
	for end < len(s) {
		r, size = utf8.DecodeRuneInString(s[end:])
		next := lookup(r).property
		if !c.joins(next) {
			break
		}
		c.add(next)
		end += size
	}
	return end
}

func (c *cluster) add(p property) {
	c.conjunctLinked = p.conjunct() == incbLinker || c.conjunctLinked && p.conjunct() == incbExtend
	c.emoji = c.emoji.after(p)
	c.regionalIndicators++
	if p.breakClass() != regionalIndicator {
		c.regionalIndicators = 0
	}
	c.last = p
}

func (e emojiTail) after(p property) emojiTail {
	switch {
	case p.pictographic(), e == emojiPictographic && p.breakClass() == extend:
		return emojiPictographic
	case e == emojiPictographic && p.breakClass() == zwj:
		return emojiJoined
	}
	return emojiNone
}

func (c *cluster) joins(next property) bool {
	return joinsPair(c.last.breakClass(), next.breakClass()) || c.continues(next)
}

func joinsPair(previous, next property) bool {
	if previous == cr {
		return next == lf
	}
	if isControlClass(previous) || isControlClass(next) {
		return false
	}
	return joinsHangul(previous, next) || joinsExtending(previous, next)
}

func isControlClass(class property) bool {
	return class == control || class == cr || class == lf
}

var hangulFollowers = [...]uint16{
	hangulL:   1<<hangulL | 1<<hangulV | 1<<hangulLV | 1<<hangulLVT,
	hangulV:   1<<hangulV | 1<<hangulT,
	hangulLV:  1<<hangulV | 1<<hangulT,
	hangulT:   1 << hangulT,
	hangulLVT: 1 << hangulT,
}

func joinsHangul(previous, next property) bool {
	return int(previous) < len(hangulFollowers) && hangulFollowers[previous]&(1<<next) != 0
}

func joinsExtending(previous, next property) bool {
	return next == extend || next == zwj || next == spacingMark || previous == prepend
}

func (c *cluster) continues(next property) bool {
	return c.conjunctLinked && next.conjunct() == incbConsonant ||
		c.emoji == emojiJoined && next.pictographic() ||
		next.breakClass() == regionalIndicator && c.regionalIndicators%2 == 1
}
