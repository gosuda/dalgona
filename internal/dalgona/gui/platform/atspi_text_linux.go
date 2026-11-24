//go:build linux

package platform

import (
	"errors"
	"image"
	"unicode/utf8"

	"github.com/godbus/dbus/v5"
	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

const (
	atspiGranularityChar uint32 = iota
	atspiGranularityWord
	atspiGranularitySentence
	atspiGranularityLine
	atspiGranularityParagraph
)

func atspiHasText(n SemanticNode) bool {
	return n.TextContainer || n.Role == RoleTextField || n.Role == RoleSecureField || n.Value != "" || n.TextState != TextReady
}

func (o *atspiObject) requireText() error {
	if o.node.TextState == TextReady {
		return nil
	}
	_, err := o.bridge.tree.QueryText(o.node.ID, o.snap.Revision())
	if err == nil {
		return nil
	}
	return dbus.Error{Name: "org.freedesktop.DBus.Error.Failed", Body: []any{err.Error()}}
}

func atspiTextOf(n SemanticNode) string {
	if n.Role == RoleSecureField {
		return ""
	}
	return n.Value
}

func atspiSelectionOf(n SemanticNode) inputtext.Range {
	if n.Role == RoleSecureField {
		return inputtext.Range{}
	}
	return n.Selection
}

func atspiCharacters(text string, offset int) (int32, error) {
	characters, err := inputtext.Convert(text, offset, inputtext.Bytes, inputtext.Scalars)
	if err != nil {
		return 0, dbus.Error{Name: "org.freedesktop.DBus.Error.Failed", Body: []any{"the published selection does not fall on a character boundary; publish a semantic tree with valid selection offsets"}}
	}
	return clampInt32(characters), nil
}

func atspiByteOffset(text string, characters int) (int, error) {
	offset, err := inputtext.Convert(text, characters, inputtext.Scalars, inputtext.Bytes)
	if err != nil {
		return 0, atspiInvalid("character offset %d is outside the text of %d characters; ask CharacterCount first", characters, inputtext.Count(text, inputtext.Scalars))
	}
	return offset, nil
}

func atspiTextRange(text string, start, end int32) (inputtext.Range, error) {
	if start > end {
		return inputtext.Range{}, atspiInvalid("the range %d to %d ends before it starts; pass the smaller offset first", start, end)
	}
	first, err := atspiByteOffset(text, int(start))
	if err != nil {
		return inputtext.Range{}, err
	}
	last, err := atspiByteOffset(text, int(end))
	if err != nil {
		return inputtext.Range{}, err
	}
	r := inputtext.Range{Start: first, End: last}
	if inputtext.CheckRange(text, r) != nil {
		return inputtext.Range{}, atspiInvalid("the range %d to %d splits a grapheme cluster; move both ends to whole displayed characters", start, end)
	}
	return r, nil
}

var atspiTextProperties = map[string]atspiProperty{
	"version": atspiConstant(uint32(1)),
	"CharacterCount": func(o *atspiObject) (any, error) {
		if err := o.requireText(); err != nil {
			return nil, err
		}
		if o.node.TextContainer {
			length, err := o.textView().length(inputtext.Scalars)
			return clampInt32(length), atspiTextError(err)
		}
		return clampInt32(inputtext.Count(atspiTextOf(o.node), inputtext.Scalars)), nil
	},
	"CaretOffset": func(o *atspiObject) (any, error) {
		if err := o.requireText(); err != nil {
			return nil, err
		}
		if o.node.TextContainer {
			_, end, err := o.groupSelection()
			return end, err
		}
		return atspiCharacters(atspiTextOf(o.node), atspiSelectionOf(o.node).End)
	},
}

var atspiTextMembers = map[string]atspiMember{
	"GetText":              {in: []any{int32(0), int32(0)}, out: []any{""}, call: atspiGetText},
	"GetStringAtOffset":    {in: []any{int32(0), uint32(0)}, out: []any{"", int32(0), int32(0)}, call: atspiStringAtOffset},
	"GetCharacterAtOffset": {in: []any{int32(0)}, out: []any{int32(0)}, call: atspiCharacterAtOffset},
	"GetNSelections":       {out: []any{int32(0)}, call: atspiSelectionCount},
	"GetSelection":         {in: []any{int32(0)}, out: []any{int32(0), int32(0)}, call: atspiTextSelection},
	"SetCaretOffset": {in: []any{int32(0)}, out: []any{false}, call: func(o *atspiObject, args []any) ([]any, error) {
		return o.selectText(args[0].(int32), args[0].(int32))
	}},
	"SetSelection": {in: []any{int32(0), int32(0), int32(0)}, out: []any{false}, call: func(o *atspiObject, args []any) ([]any, error) {
		if index := args[0].(int32); index != 0 {
			return nil, atspiInvalid("selection %d does not exist because a text field has one selection; use index 0", index)
		}
		return o.selectText(args[1].(int32), args[2].(int32))
	}},
	"AddSelection":        {in: []any{int32(0), int32(0)}, out: []any{false}, call: atspiAddSelection},
	"RemoveSelection":     {in: []any{int32(0)}, out: []any{false}, call: atspiRemoveSelection},
	"GetCharacterExtents": {in: []any{int32(0), uint32(0)}, out: []any{int32(0), int32(0), int32(0), int32(0)}, call: atspiCharacterExtents},
	"GetRangeExtents":     {in: []any{int32(0), int32(0), uint32(0)}, out: []any{int32(0), int32(0), int32(0), int32(0)}, call: atspiRangeExtents},
	"GetOffsetAtPoint":    {in: []any{int32(0), int32(0), uint32(0)}, out: []any{int32(0)}, call: atspiOffsetAtPoint},
}

func (o *atspiObject) textView() textView { return o.bridge.tree.textView(o.snap, o.node.ID) }

func atspiTextError(err error) error {
	if err == nil {
		return nil
	}
	if errors.Is(err, inputtext.InvalidOffset) {
		return atspiInvalid("the offset leaves the text or splits a character")
	}
	return dbus.Error{Name: "org.freedesktop.DBus.Error.Failed", Body: []any{err.Error()}}
}

func atspiGetText(o *atspiObject, args []any) ([]any, error) {
	if o.node.TextContainer {
		start, end := max(0, int(args[0].(int32))), int(args[1].(int32))
		if end >= 0 {
			start = min(start, end)
		}
		text, err := o.textView().readUnits(start, end, inputtext.Scalars)
		return []any{text}, atspiTextError(err)
	}
	text := atspiTextOf(o.node)
	end := int(args[1].(int32))
	if count := inputtext.Count(text, inputtext.Scalars); end < 0 || end > count {
		end = count
	}
	first, err := atspiByteOffset(text, min(max(int(args[0].(int32)), 0), end))
	if err != nil {
		return nil, err
	}
	last, err := atspiByteOffset(text, end)
	if err != nil {
		return nil, err
	}
	return []any{text[first:objectEnd(text[:last], first)]}, nil
}

func atspiStringAtOffset(o *atspiObject, args []any) ([]any, error) {
	if o.node.TextContainer {
		return atspiGroupSegment(o, args)
	}
	text := atspiTextOf(o.node)
	at, err := atspiByteOffset(text, min(max(int(args[0].(int32)), 0), inputtext.Count(text, inputtext.Scalars)))
	if err != nil {
		return nil, err
	}
	start, end, err := atspiSegment(o.node, at, args[1].(uint32))
	if err != nil {
		return nil, err
	}
	end = objectEnd(text[:end], start)
	first, err := atspiCharacters(text, start)
	if err != nil {
		return nil, err
	}
	last, err := atspiCharacters(text, end)
	if err != nil {
		return nil, err
	}
	return []any{text[start:end], first, last}, nil
}

func atspiSegment(n SemanticNode, at int, granularity uint32) (int, int, error) {
	text := atspiTextOf(n)
	var segment inputtext.Range
	var err error
	switch granularity {
	case atspiGranularityChar:
		return atspiCluster(text, at)
	case atspiGranularityWord:
		segment, err = wordSegment(text, at, letterWord)
	case atspiGranularitySentence:
		segment, err = sentenceSegment(text, at)
	case atspiGranularityLine:
		line, _ := n.lineAround(at)
		segment = line.Range
	case atspiGranularityParagraph:
		segment.Start, segment.End = atspiSpan(text, at, atspiParagraphSeparator)
	default:
		return 0, 0, atspiInvalid("text granularity %d is unknown; use 0 for characters, 1 for words, 2 for sentences, 3 for lines, or 4 for paragraphs", granularity)
	}
	if err != nil {
		return 0, 0, atspiInvalid("offset %d does not start a character; ask CharacterCount first", at)
	}
	return segment.Start, segment.End, nil
}

func atspiCluster(text string, at int) (int, int, error) {
	if at == len(text) {
		return at, at, nil
	}
	_, size := utf8.DecodeRuneInString(text[at:])
	cluster, err := inputtext.PaintRange(text, inputtext.Range{Start: at, End: at + size})
	if err != nil {
		return 0, 0, atspiInvalid("offset %d does not start a character; ask CharacterCount first", at)
	}
	return cluster.Start, cluster.End, nil
}

func atspiSpan(text string, at int, separator func(rune) bool) (int, int) {
	start := at
	for start > 0 {
		r, size := utf8.DecodeLastRuneInString(text[:start])
		if separator(r) {
			break
		}
		start -= size
	}
	end := at
	for end < len(text) {
		r, size := utf8.DecodeRuneInString(text[end:])
		end += size
		if separator(r) {
			break
		}
	}
	return start, end
}

func atspiParagraphSeparator(r rune) bool {
	return r == '\n' || r == '\r' || r >= '\u001c' && r <= '\u001e' || r == '\u0085' || r == '\u2029'
}

func atspiCharacterAtOffset(o *atspiObject, args []any) ([]any, error) {
	if o.node.TextContainer {
		return atspiGroupCharacter(o, int(args[0].(int32)))
	}
	text, offset := atspiTextOf(o.node), int(args[0].(int32))
	at, err := atspiByteOffset(text, offset)
	if err != nil || at == len(text) {
		return nil, atspiInvalid("character offset %d is outside the text of %d characters; ask CharacterCount first", offset, inputtext.Count(text, inputtext.Scalars))
	}
	r, _ := utf8.DecodeRuneInString(text[at:])
	return []any{r}, nil
}

func atspiCharacterExtents(o *atspiObject, args []any) ([]any, error) {
	if o.node.TextContainer {
		return atspiGroupCharacterBox(o, args)
	}
	at, err := atspiByteOffset(atspiTextOf(o.node), int(args[0].(int32)))
	if err != nil {
		return nil, err
	}
	box, ok := o.node.characterBox(at)
	return o.textBox(box, ok, args[1].(uint32))
}

func atspiRangeExtents(o *atspiObject, args []any) ([]any, error) {
	if o.node.TextContainer {
		return atspiGroupRangeBox(o, args)
	}
	text, first, last := atspiTextOf(o.node), args[0].(int32), args[1].(int32)
	start, err := atspiByteOffset(text, int(min(first, last)))
	if err != nil {
		return nil, err
	}
	end, err := atspiByteOffset(text, int(max(first, last)))
	if err != nil {
		return nil, err
	}
	box, ok := o.node.rangeBox(inputtext.Range{Start: start, End: end})
	return o.textBox(box, ok, args[2].(uint32))
}

func (o *atspiObject) textBox(box image.Rectangle, ok bool, coord uint32) ([]any, error) {
	offset, err := o.offset(coord)
	if err != nil {
		return nil, err
	}
	if !ok {
		return []any{int32(-1), int32(-1), int32(-1), int32(-1)}, nil
	}
	r := box.Add(offset)
	return []any{clampInt32(r.Min.X), clampInt32(r.Min.Y), clampInt32(r.Dx()), clampInt32(r.Dy())}, nil
}

func atspiOffsetAtPoint(o *atspiObject, args []any) ([]any, error) {
	p, err := o.point(args)
	if err != nil {
		return nil, err
	}
	if o.node.TextContainer {
		return atspiGroupOffsetAtPoint(o, p)
	}
	line, ok := o.node.closestLine(p)
	if !ok || !p.In(line.Bounds) {
		return []any{int32(-1)}, nil
	}
	characters, err := atspiCharacters(atspiTextOf(o.node), line.nearest(o.node.Value, p.X))
	if err != nil {
		return nil, err
	}
	return []any{characters}, nil
}

func (o *atspiObject) groupSelection() (int32, int32, error) {
	v := o.textView()
	r := v.selected()
	if r.Start.Node == "" {
		return 0, 0, nil
	}
	start, end, err := v.unitOffsets(r, inputtext.Scalars)
	return clampInt32(start), clampInt32(end), atspiTextError(err)
}

func (o *atspiObject) characterSelection() (int32, int32, error) {
	if o.node.TextContainer {
		return o.groupSelection()
	}
	return atspiCharacterRange(o.node)
}

func atspiCharacterRange(n SemanticNode) (int32, int32, error) {
	text, selection := atspiTextOf(n), atspiSelectionOf(n)
	start, err := atspiCharacters(text, selection.Start)
	if err != nil {
		return 0, 0, err
	}
	end, err := atspiCharacters(text, selection.End)
	return start, end, err
}

func atspiSelectionCount(o *atspiObject, _ []any) ([]any, error) {
	start, end, err := o.characterSelection()
	if err != nil {
		return nil, err
	}
	if start == end {
		return []any{int32(0)}, nil
	}
	return []any{int32(1)}, nil
}

func atspiTextSelection(o *atspiObject, args []any) ([]any, error) {
	start, end, err := o.characterSelection()
	if err != nil {
		return nil, err
	}
	if index := args[0].(int32); index != 0 || start == end {
		return nil, atspiInvalid("selection %d does not exist; ask GetNSelections first", index)
	}
	return []any{start, end}, nil
}

func atspiAddSelection(o *atspiObject, args []any) ([]any, error) {
	start, end, err := o.characterSelection()
	if err != nil {
		return nil, err
	}
	if start != end {
		return []any{false}, nil
	}
	return o.selectText(args[0].(int32), args[1].(int32))
}

func atspiRemoveSelection(o *atspiObject, args []any) ([]any, error) {
	start, end, err := o.characterSelection()
	if err != nil {
		return nil, err
	}
	if args[0].(int32) != 0 || start == end {
		return []any{false}, nil
	}
	return o.selectText(end, end)
}

func (o *atspiObject) selectText(start, end int32) ([]any, error) {
	if o.node.TextContainer {
		err := o.textView().selectUnits(int(start), int(end), inputtext.Scalars)
		return []any{err == nil}, atspiTextError(err)
	}
	if o.node.Role == RoleSecureField {
		return nil, dbus.Error{Name: "org.freedesktop.DBus.Error.AccessDenied", Body: []any{"the secure field hides its text, so its caret and selection stay private; move the caret with the keyboard"}}
	}
	r, err := atspiTextRange(o.node.Value, start, end)
	if err != nil {
		return nil, err
	}
	return o.request(ActionSelectText, "", r)
}

var atspiEditableTextMembers = map[string]atspiMember{
	"SetTextContents": {in: []any{""}, out: []any{false}, call: func(o *atspiObject, args []any) ([]any, error) {
		value := args[0].(string)
		return o.edit(value, len(value))
	}},
	"InsertText": {in: []any{int32(0), "", int32(0)}, out: []any{false}, call: atspiInsertText},
	"DeleteText": {in: []any{int32(0), int32(0)}, out: []any{false}, call: func(o *atspiObject, args []any) ([]any, error) {
		text := o.node.Value
		removed, err := atspiTextRange(text, args[0].(int32), args[1].(int32))
		if err != nil {
			return nil, err
		}
		return o.edit(text[:removed.Start]+text[removed.End:], removed.Start)
	}},
}

func atspiInsertText(o *atspiObject, args []any) ([]any, error) {
	text, position := o.node.Value, args[0].(int32)
	at, err := atspiTextRange(text, position, position)
	if err != nil {
		return nil, err
	}
	inserted, err := atspiLeadingBytes(args[1].(string), args[2].(int32))
	if err != nil {
		return nil, err
	}
	return o.edit(text[:at.Start]+inserted+text[at.Start:], at.Start+len(inserted))
}

func atspiLeadingBytes(text string, length int32) (string, error) {
	if length < 0 {
		return "", atspiInvalid("length %d is negative; pass the byte length of the text to insert", length)
	}
	if int(length) >= len(text) {
		return text, nil
	}
	if prefix := text[:length]; utf8.ValidString(prefix) {
		return prefix, nil
	}
	return "", atspiInvalid("length %d bytes ends inside a UTF-8 sequence; pass the byte length of whole characters", length)
}

func (o *atspiObject) edit(value string, caret int) ([]any, error) {
	if !utf8.ValidString(value) {
		return nil, atspiInvalid("the edited text is not valid UTF-8; send UTF-8 text")
	}
	return o.request(ActionSetValue, value, inputtext.Range{Start: caret, End: caret})
}
