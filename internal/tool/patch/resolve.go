package patch

import (
	"bytes"
	"cmp"
	"context"
	"errors"
	"fmt"
	"io/fs"
	"slices"
	"strings"

	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type fileResolver struct {
	style Style
	group fileGroup
	env   tool.Env
	file  *stagedFile
}

func (r fileResolver) stageContent(ctx context.Context, doc document) error {
	content := r.group.edits
	if last := content[len(content)-1]; last.kind == editRename {
		if err := r.stageRename(ctx, last); err != nil {
			return err
		}
		content = content[:len(content)-1]
	}
	if r.style == ApplyPatch {
		return r.applySequential(content)
	}
	return r.applySimultaneous(doc, content)
}

func (r fileResolver) stageRename(ctx context.Context, item edit) error {
	display, absolute, err := resolveEnvPath(ctx, r.env, item.to)
	if err != nil {
		return err
	}
	if hasParentComponent(item.to) {
		return errors.New("patch: rename destination contains parent traversal")
	}
	if _, statErr := r.env.Runner.Stat(ctx, absolute); statErr == nil {
		return fmt.Errorf("patch: cannot rename %s to %s: destination already exists", r.group.display, display)
	} else if !errors.Is(statErr, fs.ErrNotExist) {
		return fmt.Errorf("patch: cannot inspect rename destination %s: %w", display, statErr)
	}
	if item.reference != "" {
		if proofErr := proveUnchanged(r.env.Snapshots, item.reference, r.group.display, r.file.before); proofErr != nil {
			return proofErr
		}
	}
	if r.file.renamePin, err = pinVerifiedComponents(r.env, display, absolute); err != nil {
		return err
	}
	r.file.renameTo, r.file.renameAbs = display, absolute
	return nil
}

func (r fileResolver) applySimultaneous(doc document, edits []edit) error {
	var replacements []replacement
	for _, item := range edits {
		resolved, err := r.resolve(doc, item)
		if err != nil {
			return err
		}
		replacements = append(replacements, resolved...)
	}
	if err := checkOverlaps(r.style, r.group.display, replacements); err != nil {
		return err
	}
	slices.SortStableFunc(replacements, func(a, b replacement) int {
		return cmp.Or(cmp.Compare(b.start, a.start), cmp.Compare(b.index, a.index))
	})
	file := r.file
	file.after = bytes.Clone(file.before)
	for _, item := range replacements {
		if item.noop(file.before) {
			continue
		}
		file.after = spliceBytes(file.after, item)
		file.splices = append(file.splices, item.splice)
	}
	return nil
}

func (r replacement) noop(before []byte) bool {
	if r.start == r.end {
		return len(r.body) == 0
	}
	return bytes.Equal(before[r.start:r.end], r.body)
}

func (r fileResolver) applySequential(edits []edit) error {
	r.file.after = bytes.Clone(r.file.before)
	r.file.sound = false
	for _, item := range edits {
		doc, err := decodeDocument(r.group.display, r.file.after)
		if err != nil {
			return err
		}
		resolved, err := r.resolve(doc, item)
		if err != nil {
			return err
		}
		for _, replacement := range resolved {
			r.file.after = spliceBytes(r.file.after, replacement)
		}
	}
	return nil
}

func spliceBytes(before []byte, item replacement) []byte {
	after := make([]byte, 0, len(before)-(item.end-item.start)+len(item.body))
	after = append(after, before[:item.start]...)
	after = append(after, item.body...)
	after = append(after, before[item.end:]...)
	return after
}

func (r fileResolver) resolve(doc document, item edit) ([]replacement, error) {
	if item.kind != editChange {
		return nil, nil
	}
	if err := proveTag(r.file, item); err != nil {
		return nil, err
	}
	switch item.locator.kind {
	case locatorWhole:
		return r.resolveWhole(doc, item)
	case locatorLines, locatorSpan:
		return r.resolveLines(doc, item)
	case locatorGap:
		return r.resolveGap(doc, item)
	case locatorText:
		return r.resolveText(doc, item)
	default:
		return nil, fmt.Errorf("patch: changes[%d]: unsupported locator", item.index)
	}
}

func proveTag(file *stagedFile, item edit) error {
	if item.guard.kind != guardWholeTag && item.guard.kind != guardVersion {
		return nil
	}
	expected := item.guard.text
	whole, version := snapshot.Tag("whole", file.before), snapshot.VersionTag(file.before)
	if expected == whole || expected == version {
		return nil
	}
	current := whole
	if len(expected) == 4 {
		current = version
	}
	return fmt.Errorf("patch: changes[%d]: tag %s does not match %s now (tag %s). The file changed since you read it; read it again", item.index, expected, file.path, current)
}

func (r fileResolver) resolveWhole(doc document, item edit) ([]replacement, error) {
	if item.guard.kind == guardSeen && doc.lines() > 0 {
		if err := r.proveSeen(1, doc.lines(), item.index); err != nil {
			return nil, err
		}
	}
	body := doc.render(item.body)
	if doc.bom {
		body = append([]byte{0xef, 0xbb, 0xbf}, body...)
	}
	return []replacement{{
		index: item.index, start: 0, end: len(r.file.before), body: body,
		splice: snapshot.Splice{Start: 0, End: doc.lines(), Inserted: countBodyLines(body)},
	}}, nil
}

func (r fileResolver) resolveLines(doc document, item edit) ([]replacement, error) {
	first, last, err := r.targetLines(item)
	if err != nil {
		return nil, err
	}
	if first < 1 || last < first || last > doc.lines() {
		return nil, fmt.Errorf("patch: line %d does not exist (file has %d lines)", last, doc.lines())
	}
	if quoteErr := proveQuotedSpan(doc, item, r.group.display); quoteErr != nil {
		return nil, quoteErr
	}
	start, end := doc.lineRange(first, last)
	body := doc.render(item.body)
	if r.needsCoverage(item, r.file.before[start:end], body) {
		if coverErr := proveCoverage(r.env.Snapshots, item, r.file.path, item.locator.first, item.locator.last); coverErr != nil {
			return nil, coverErr
		}
	}
	return []replacement{placeLines(doc, item, first, last, body)}, nil
}

func (r fileResolver) targetLines(item edit) (int, int, error) {
	first, last := item.locator.first, item.locator.last
	if item.locator.kind == locatorSpan {
		if err := r.proveSeen(first, last, item.index); err != nil {
			return 0, 0, err
		}
	}
	if item.guard.kind != guardReference {
		return first, last, nil
	}
	mapped, err := mapReferenceSpan(r.env.Snapshots, item.guard.text, r.file, first, last)
	if err != nil {
		return 0, 0, err
	}
	return mapped.Start + 1, mapped.End, nil
}

func (r fileResolver) needsCoverage(item edit, current, body []byte) bool {
	return r.style == HashlineEnhanced && item.guard.kind == guardReference && item.action == actionReplace && !bytes.Equal(current, body)
}

func placeLines(doc document, item edit, first, last int, body []byte) replacement {
	start, end := doc.lineRange(first, last)
	inserted := countBodyLines(body)
	switch item.action {
	case actionInsertBefore:
		return replacement{index: item.index, start: start, end: start, body: body, splice: snapshot.Splice{Start: first - 1, End: first - 1, Inserted: inserted}}
	case actionInsertAfter:
		return replacement{index: item.index, start: end, end: end, body: body, splice: snapshot.Splice{Start: last, End: last, Inserted: inserted}}
	case actionReplace:
	}
	return replacement{index: item.index, start: start, end: end, body: body, splice: snapshot.Splice{Start: first - 1, End: last, Inserted: inserted}}
}

func (r fileResolver) resolveGap(doc document, item edit) ([]replacement, error) {
	gap, err := r.targetGap(doc, item)
	if err != nil {
		return nil, err
	}
	if gap < 0 || gap > doc.lines() {
		return nil, fmt.Errorf("patch: line %d does not exist (file has %d lines)", item.locator.before, doc.lines())
	}
	body := doc.render(item.body)
	splice := snapshot.Splice{Start: gap, End: gap, Inserted: countBodyLines(body)}
	if gap == doc.lines() && !doc.finalNewline && doc.lines() > 0 {
		if err := r.proveFinalRowCoverage(item, doc.lines()); err != nil {
			return nil, err
		}
		splice.Start = gap - 1
	}
	offset := doc.gapOffset(gap)
	return []replacement{{index: item.index, start: offset, end: offset, body: body, splice: splice}}, nil
}

func (r fileResolver) targetGap(doc document, item edit) (int, error) {
	gap := item.locator.before - 1
	if item.locator.before == tailGap {
		gap = doc.lines()
	}
	if item.guard.kind != guardReference {
		return gap, nil
	}
	return mapReferenceGap(r.env.Snapshots, item.guard.text, r.file, gap)
}

func (r fileResolver) proveFinalRowCoverage(item edit, line int) error {
	if r.style != HashlineEnhanced {
		return nil
	}
	return proveCoverage(r.env.Snapshots, item, r.file.path, line, line)
}

func (r fileResolver) resolveText(doc document, item edit) ([]replacement, error) {
	matches, err := r.selectMatches(doc, item)
	if err != nil {
		return nil, err
	}
	r.file.sound = false
	body := doc.render(numberedBody(item.body, matches[0].prefixStart))
	out := make([]replacement, 0, len(matches))
	for _, match := range matches {
		start, end := doc.rawOffset(match.start), doc.rawOffset(match.end)
		switch item.action {
		case actionInsertBefore:
			end = start
		case actionInsertAfter:
			start = end
		case actionReplace:
		}
		out = append(out, replacement{index: item.index, start: start, end: end, body: body})
	}
	return out, nil
}

func (r fileResolver) selectMatches(doc document, item edit) ([]textMatch, error) {
	display := r.group.display
	matches := findTextMatches(string(doc.view), item.locator.old, item.locator.context, item.locator.atEOF)
	if len(matches) == 0 {
		return nil, fmt.Errorf("patch: changes[%d]: old is not in %s. Copy it again or read the file", item.index, display)
	}
	if item.locator.lineHint > 0 {
		matches = matchesAtLine(matches, doc, item.locator.lineHint)
		if len(matches) == 0 {
			return nil, fmt.Errorf("patch: changes[%d]: old does not start at line %d", item.index, item.locator.lineHint)
		}
	}
	if !item.locator.all && len(matches) != 1 {
		return nil, fmt.Errorf("patch: changes[%d]: old occurs %d times in %s. Resend with a line hint", item.index, len(matches), display)
	}
	if item.locator.all && item.guard.kind == guardSeen {
		if err := r.proveSeen(1, doc.lines(), item.index); err != nil {
			return nil, err
		}
	}
	return matches, nil
}

func numberedBody(body string, prefix int) string {
	if prefix == 0 {
		return body
	}
	start, stripped, ok := stripNumberedPrefixes(strings.ReplaceAll(body, "\r\n", "\n"))
	if ok && start == prefix {
		return stripped
	}
	return body
}

func matchesAtLine(matches []textMatch, doc document, line int) []textMatch {
	if line < 1 || line > doc.lines() {
		return nil
	}
	start := doc.starts[line-1]
	end := len(doc.view)
	if line < doc.lines() {
		end = doc.starts[line]
	}
	out := matches[:0]
	for _, match := range matches {
		if match.start >= start && match.start < end {
			out = append(out, match)
		}
	}
	return out
}

func mapReferenceSpan(store *snapshot.Store, ref string, file *stagedFile, first, last int) (snapshot.Span, error) {
	if store == nil {
		return snapshot.Span{}, errors.New("patch: snapshot store is unavailable")
	}
	mapped, err := store.ResolveSpan(ref, file.path, file.before, snapshot.Span{Start: first - 1, End: last})
	if err != nil {
		return snapshot.Span{}, fmt.Errorf("patch: reference %s for %s is stale: %w. Read again", ref, file.path, err)
	}
	return mapped, nil
}

func mapReferenceGap(store *snapshot.Store, ref string, file *stagedFile, gap int) (int, error) {
	if store == nil {
		return 0, errors.New("patch: snapshot store is unavailable")
	}
	mapped, err := store.ResolveGap(ref, file.path, file.before, gap)
	if err != nil {
		return 0, fmt.Errorf("patch: reference %s for %s is stale: %w. Read again", ref, file.path, err)
	}
	return mapped, nil
}

func proveUnchanged(store *snapshot.Store, ref, path string, before []byte) error {
	if store == nil || !store.Unchanged(ref, path, before) {
		return fmt.Errorf("patch: reference %s for %s is expired or the file changed. Read again", ref, path)
	}
	return nil
}

func (r fileResolver) proveSeen(first, last, index int) error {
	if r.env.Snapshots != nil && r.env.Snapshots.Seen(r.file.path, r.file.before, first, last) {
		return nil
	}
	return fmt.Errorf("patch: changes[%d]: lines %d-%d of %s were not displayed in this session", index, first, last, r.file.path)
}

func proveCoverage(store *snapshot.Store, item edit, path string, first, last int) error {
	if store != nil && store.Covers(item.guard.text, first, last) {
		return nil
	}
	return fmt.Errorf("patch: changes[%d]: lines %d-%d of %s need prior observation. Read them, then resend", item.index, first, last, path)
}

func proveQuotedSpan(doc document, item edit, path string) error {
	if item.locator.kind != locatorSpan {
		return nil
	}
	rows := item.locator.quoted
	first, last := item.locator.first, item.locator.last
	lines := []int{first, last}
	if len(rows) == last-first+1 {
		lines = make([]int, 0, len(rows))
		for line := first; line <= last; line++ {
			lines = append(lines, line)
		}
	}
	for i, line := range lines {
		if canonicalLine(rows[i]) != canonicalLine(doc.lineText(line)) {
			return fmt.Errorf("patch: changes[%d]: line %d of %s is not the quoted text. Copy it again or read the file", item.index, line, path)
		}
	}
	return nil
}

func checkOverlaps(style Style, path string, replacements []replacement) error {
	for i, left := range replacements {
		for _, right := range replacements[i+1:] {
			if err := overlapError(style, path, left, right); err != nil {
				return err
			}
		}
	}
	return nil
}

func overlapError(style Style, path string, left, right replacement) error {
	if left.index == right.index {
		return nil
	}
	if max(left.start, right.start) < min(left.end, right.end) {
		return fmt.Errorf("patch: changes[%d] and changes[%d] overlap in %s", left.index, right.index, path)
	}
	if style.profile() && insertionConflict(left, right) {
		return fmt.Errorf("patch: changes[%d] and changes[%d] have conflicting insertion gaps in %s", left.index, right.index, path)
	}
	return nil
}

func insertionConflict(left, right replacement) bool {
	switch {
	case left.start == left.end:
		return right.start <= left.start && left.start <= right.end
	case right.start == right.end:
		return left.start <= right.start && right.start <= left.end
	default:
		return false
	}
}
