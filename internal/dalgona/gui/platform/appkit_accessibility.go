package platform

import (
	"encoding/binary"
	"errors"
	"fmt"
	"image"
	"io"
	"math"
	"unicode/utf16"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

const (
	appKitNone         = math.MaxUint32
	appKitIntentHeader = 28
)

type appKitRole uint8

const (
	appKitGroup appKitRole = iota + 1
	appKitButton
	appKitCheckBox
	appKitRadioButton
	appKitTextField
	appKitTextArea
	appKitStaticText
	appKitLink
	appKitMenuItem
	appKitMenu
	appKitHeading
	appKitList
	appKitRow
	appKitTable
	appKitCell
	appKitImage
)

type appKitSubrole uint8

const (
	appKitNoSubrole appKitSubrole = iota
	appKitSwitch
	appKitSecureTextField
	appKitContentList
	appKitTableRow
	appKitDialog
)

const (
	appKitDisabled uint8 = 1 << iota
	appKitChecked
	appKitPressable
	appKitEditable
	appKitText
	appKitCheckable
	appKitColumnHeader
	appKitRowHeader
)

const (
	appKitSelectable uint32 = 1 << iota
	appKitSelected
	appKitExpandable
	appKitExpanded
	appKitTextUncached
	appKitTextLoading
	appKitTextFailed
	appKitTextContainer
	appKitTextSelectable
)

var appKitTextStates = [...]uint32{
	TextReady:    0,
	TextUncached: appKitTextUncached,
	TextLoading:  appKitTextLoading,
	TextFailed:   appKitTextFailed,
}

func appKitStates(n SemanticNode) uint32 {
	var flags uint32
	if n.Selectable || n.Role == RoleRadioButton {
		flags |= appKitSelectable
	}
	if n.selected() {
		flags |= appKitSelected
	}
	if n.Expandable {
		flags |= appKitExpandable
	}
	if n.Expanded {
		flags |= appKitExpanded
	}
	if n.TextContainer {
		flags |= appKitTextContainer
	}
	if n.TextSelectable {
		flags |= appKitTextSelectable
	}
	return flags | appKitTextStates[n.TextState]
}

type appKitTraits struct {
	role      appKitRole
	subrole   appKitSubrole
	abilities uint8
}

var appKitRoles = [...]appKitTraits{
	RoleButton:       {appKitButton, appKitNoSubrole, appKitPressable},
	RoleCheckBox:     {appKitCheckBox, appKitNoSubrole, appKitPressable | appKitCheckable},
	RoleRadioButton:  {appKitRadioButton, appKitNoSubrole, appKitPressable | appKitCheckable},
	RoleSwitch:       {appKitCheckBox, appKitSwitch, appKitPressable | appKitCheckable},
	RoleTextField:    {appKitTextField, appKitNoSubrole, appKitEditable | appKitText},
	RoleSecureField:  {appKitTextField, appKitSecureTextField, appKitEditable | appKitText},
	RoleLink:         {appKitLink, appKitNoSubrole, appKitPressable},
	RoleMenuItem:     {appKitMenuItem, appKitNoSubrole, appKitPressable},
	RoleDocument:     {appKitGroup, appKitNoSubrole, 0},
	RoleHeading:      {appKitHeading, appKitNoSubrole, 0},
	RoleList:         {appKitList, appKitContentList, 0},
	RoleListItem:     {appKitGroup, appKitNoSubrole, 0},
	RoleTable:        {appKitTable, appKitNoSubrole, 0},
	RoleRow:          {appKitRow, appKitTableRow, 0},
	RoleColumnHeader: {appKitCell, appKitNoSubrole, appKitColumnHeader},
	RoleRowHeader:    {appKitCell, appKitNoSubrole, appKitRowHeader},
	RoleCell:         {appKitCell, appKitNoSubrole, 0},
	RoleCode:         {appKitStaticText, appKitNoSubrole, appKitText},
	RoleMath:         {appKitStaticText, appKitNoSubrole, appKitText},
	RoleFigure:       {appKitGroup, appKitNoSubrole, 0},
	RoleImage:        {appKitImage, appKitNoSubrole, 0},
	RoleTerminal:     {appKitTextArea, appKitNoSubrole, appKitText},
	RoleDialog:       {appKitGroup, appKitDialog, 0},
	RoleMenu:         {appKitMenu, appKitNoSubrole, 0},
	RoleStatus:       {appKitStaticText, appKitNoSubrole, appKitText},
	RoleError:        {appKitStaticText, appKitNoSubrole, appKitText},
}

var appKitTextDocument = appKitTraits{appKitStaticText, appKitNoSubrole, appKitText}

func appKitNodeTraits(n SemanticNode, leaf bool) appKitTraits {
	if n.Role == RoleDocument && n.appKitLeafText(leaf) {
		return appKitTextDocument
	}
	traits := appKitRoles[n.Role]
	if n.TextContainer || n.TextGroup != "" {
		traits.abilities |= appKitText
	}
	if n.Disabled {
		traits.abilities |= appKitDisabled
	}
	if n.Checked && traits.abilities&appKitCheckable != 0 {
		traits.abilities |= appKitChecked
	}
	return traits
}

func (n SemanticNode) appKitLeafText(leaf bool) bool {
	return leaf || n.TextState != TextReady || n.Value != ""
}

type appKitSnapshotEncoder struct {
	buf   []byte
	order []NodeID
	stack []NodeID
	index map[NodeID]uint32
	cells map[NodeID]tableCell
	laid  []appKitLine
	parts map[NodeID]int
}

type appKitLine struct {
	index uint32
	at    uint32
	line  TextLine
}

func (e *appKitSnapshotEncoder) encode(s *SemanticSnapshot) ([]byte, error) {
	e.walk(s)
	if len(e.order) >= appKitNone {
		return nil, errors.New("publish the AppKit accessibility tree: the tree has more than 4294967294 nodes; virtualize the semantic tree")
	}
	if e.index == nil {
		e.index = make(map[NodeID]uint32, len(e.order))
	}
	clear(e.index)
	for i, id := range e.order {
		e.index[id] = uint32(i) //nolint:gosec // the node count was checked against the uint32 range above
	}
	e.placeCells(s)
	e.parts = s.textOrder
	e.buf = binary.LittleEndian.AppendUint64(e.buf[:0], s.Revision())
	e.buf = binary.LittleEndian.AppendUint32(e.buf, uint32(len(e.order))) //nolint:gosec // the node count was checked against the uint32 range above
	e.buf = binary.LittleEndian.AppendUint32(e.buf, e.reference(s.Focus()))
	for _, id := range e.order {
		n, _ := s.Node(id)
		if err := e.appendNode(n, len(s.Children(id)) == 0); err != nil {
			return nil, err
		}
	}
	return e.buf, nil
}

func (e *appKitSnapshotEncoder) walk(s *SemanticSnapshot) {
	e.order, e.stack = e.order[:0], append(e.stack[:0], s.Root())
	for len(e.stack) > 0 {
		id := e.stack[len(e.stack)-1]
		e.stack = e.stack[:len(e.stack)-1]
		e.order = append(e.order, id)
		children := s.Children(id)
		for i := len(children) - 1; i >= 0; i-- {
			e.stack = append(e.stack, children[i])
		}
	}
}

func (e *appKitSnapshotEncoder) reference(id NodeID) uint32 {
	if i, ok := e.index[id]; ok && id != "" {
		return i
	}
	return appKitNone
}

func (e *appKitSnapshotEncoder) placeCells(s *SemanticSnapshot) {
	if e.cells == nil {
		e.cells = make(map[NodeID]tableCell)
	}
	clear(e.cells)
	for _, id := range e.order {
		if n, _ := s.Node(id); n.Role != RoleTable {
			continue
		}
		for _, c := range s.table(id).cells {
			e.cells[c.id] = c
		}
	}
}

func (e *appKitSnapshotEncoder) appendNode(n SemanticNode, leaf bool) error {
	help := n.description()
	for _, text := range [...]string{string(n.ID), n.Name, help, n.Value, n.Placeholder, n.TextBefore} {
		if len(text) > transferLimit {
			return fmt.Errorf("publish the AppKit accessibility tree: node %q holds text over 32 MiB; split its text into TextObjects of at most 64 KiB", n.ID)
		}
	}
	traits := appKitNodeTraits(n, leaf)
	location, length := appKitSelection(n.Value, n.Selection)
	b := binary.LittleEndian.AppendUint32(e.buf, e.reference(n.Parent))
	b = binary.LittleEndian.AppendUint32(b, e.reference(n.Target))
	b = append(b, byte(traits.role), byte(traits.subrole), traits.abilities, byte(min(max(n.Level, 0), math.MaxUint8))) //nolint:gosec // the level is clamped to the byte range
	b = appendAppKitRect(b, n.Bounds)
	b = binary.LittleEndian.AppendUint32(b, location)
	b = binary.LittleEndian.AppendUint32(b, length)
	b = binary.LittleEndian.AppendUint32(b, uint32(len(n.ID))) //nolint:gosec // the identity length was checked against transferLimit
	counts := len(b)
	b = append(b, make([]byte, 12)...)
	b = e.appendPlacement(b, n.ID)
	tally := len(b)
	b = append(b, make([]byte, 8)...)
	extras := len(b)
	b = binary.LittleEndian.AppendUint32(b, 0)
	b = binary.LittleEndian.AppendUint32(b, appKitStates(n))
	b = binary.LittleEndian.AppendUint32(b, e.reference(n.TextGroup))
	b = binary.LittleEndian.AppendUint32(b, appKitIndex(e.parts[n.ID]))
	prefix := len(b)
	b = binary.LittleEndian.AppendUint32(b, 0)
	b = append(b, n.ID...)
	for i, text := range [...]string{n.Name, help, n.Value} {
		start := len(b)
		b = appendAppKitText(b, text)
		binary.LittleEndian.PutUint32(b[counts+4*i:], uint32((len(b)-start)/2)) //nolint:gosec // each text was checked against transferLimit, so its UTF-16 length fits
	}
	start := len(b)
	b = appendAppKitText(b, n.Placeholder)
	binary.LittleEndian.PutUint32(b[extras:], uint32((len(b)-start)/2)) //nolint:gosec // the placeholder length was checked against transferLimit
	start = len(b)
	b = appendAppKitText(b, n.TextBefore)
	binary.LittleEndian.PutUint32(b[prefix:], appKitIndex((len(b)-start)/2))
	if traits.abilities&appKitText != 0 {
		b = e.appendLines(b, tally, n)
	}
	e.buf = b
	return nil
}

func appendAppKitRect(b []byte, r image.Rectangle) []byte {
	for _, edge := range [...]int{r.Min.X, r.Min.Y, r.Max.X, r.Max.Y} {
		b = appendAppKitEdge(b, edge)
	}
	return b
}

func appendAppKitEdge(b []byte, edge int) []byte {
	return binary.LittleEndian.AppendUint32(b, uint32(clampInt32(edge))) //nolint:gosec // the edge is clamped to the int32 range and sent as its two's complement bits
}

func appKitIndex(v int) uint32 {
	return uint32(min(max(int64(v), 0), appKitNone-1)) //nolint:gosec // the index is clamped below the reserved none value
}

func (e *appKitSnapshotEncoder) appendPlacement(b []byte, id NodeID) []byte {
	place := [4]uint32{appKitNone, 0, appKitNone, 0}
	if c, ok := e.cells[id]; ok {
		place = [4]uint32{appKitIndex(c.rows.first), appKitIndex(c.rows.count), appKitIndex(c.columns.first), appKitIndex(c.columns.count)}
	}
	for _, v := range place {
		b = binary.LittleEndian.AppendUint32(b, v)
	}
	return b
}

type utf16Cursor struct {
	text  string
	at    int
	units int
}

func (c *utf16Cursor) to(offset int) uint32 {
	c.units += inputtext.Count(c.text[c.at:offset], inputtext.UTF16Units)
	c.at = offset
	return appKitIndex(c.units)
}

func (e *appKitSnapshotEncoder) appendLines(b []byte, tally int, n SemanticNode) []byte {
	e.laid = e.laid[:0]
	cursor := utf16Cursor{text: n.Value}
	var count uint32
	for line, laidOut := range n.textLines() {
		at := cursor.to(line.Range.Start)
		b = binary.LittleEndian.AppendUint32(b, at)
		if laidOut {
			e.laid = append(e.laid, appKitLine{index: count, at: at, line: line})
		}
		count++
	}
	binary.LittleEndian.PutUint32(b[tally:], count)
	binary.LittleEndian.PutUint32(b[tally+4:], appKitIndex(len(e.laid)))
	for _, l := range e.laid {
		b = appendAppKitBox(b, n.Value, l)
	}
	return b
}

func appendAppKitBox(b []byte, text string, l appKitLine) []byte {
	b = binary.LittleEndian.AppendUint32(b, l.index)
	b = appendAppKitRect(b, l.line.Bounds)
	b = binary.LittleEndian.AppendUint32(b, appKitIndex(len(l.line.Clusters)))
	cursor := utf16Cursor{text: text, at: l.line.Range.Start, units: int(l.at)}
	for cluster, span := range l.line.clusters(text) {
		b = binary.LittleEndian.AppendUint32(b, cursor.to(cluster.Start))
		b = appendAppKitEdge(b, span.Left)
		b = appendAppKitEdge(b, span.Right)
	}
	return b
}

func appKitSelection(value string, r inputtext.Range) (location, length uint32) {
	start, startErr := inputtext.Convert(value, r.Start, inputtext.Bytes, inputtext.UTF16Units)
	end, endErr := inputtext.Convert(value, r.End, inputtext.Bytes, inputtext.UTF16Units)
	if startErr != nil || endErr != nil || start > end {
		return appKitNone, 0
	}
	return uint32(start), uint32(end - start) //nolint:gosec // UTF-16 offsets of a text below transferLimit fit in uint32
}

func appendAppKitText(b []byte, text string) []byte {
	for _, r := range text {
		if r1, r2 := utf16.EncodeRune(r); r1 != '\uFFFD' || r2 != '\uFFFD' {
			b = binary.LittleEndian.AppendUint16(b, uint16(r1)) //nolint:gosec // EncodeRune returns a 16-bit high surrogate
			b = binary.LittleEndian.AppendUint16(b, uint16(r2)) //nolint:gosec // EncodeRune returns a 16-bit low surrogate
			continue
		}
		b = binary.LittleEndian.AppendUint16(b, uint16(r)) //nolint:gosec // a rune outside the surrogate-pair range is one UTF-16 unit
	}
	return b
}

type appKitPriority uint8

const (
	appKitPriorityLow appKitPriority = iota + 1
	appKitPriorityMedium
	appKitPriorityHigh
)

func announcementPriority(kind AnnouncementKind) appKitPriority {
	switch kind {
	case AnnounceProgress:
		return appKitPriorityLow
	case AnnounceCompletion:
		return appKitPriorityMedium
	case AnnounceDecision, AnnounceFailure:
		return appKitPriorityHigh
	}
	return appKitPriorityMedium
}

type appKitIntent struct {
	revision uint64
	action   AccessibilityAction
	location uint32
	length   uint32
	node     NodeID
	value    []uint16
}

var errAppKitIntent = errors.New("the AppKit bridge sent a malformed accessibility action; reopen the window to rebuild the bridge")

func readAppKitIntent(r io.Reader, header *[appKitIntentHeader]byte) (appKitIntent, error) {
	if _, err := io.ReadFull(r, header[:]); err != nil {
		return appKitIntent{}, err
	}
	h := header[:]
	i := appKitIntent{
		revision: binary.LittleEndian.Uint64(h),
		action:   AccessibilityAction(h[8]),
		location: binary.LittleEndian.Uint32(h[12:]),
		length:   binary.LittleEndian.Uint32(h[16:]),
	}
	idLength, units := binary.LittleEndian.Uint32(h[20:]), binary.LittleEndian.Uint32(h[24:])
	if i.action < ActionActivate || i.action > ActionLoadText || idLength > transferLimit || units > transferLimit/2 {
		return appKitIntent{}, errAppKitIntent
	}
	payload := make([]byte, int(idLength)+2*int(units))
	if read, err := io.ReadFull(r, payload); err != nil {
		return appKitIntent{}, fmt.Errorf("%w: the record ended after %d of %d payload bytes", errAppKitIntent, read, len(payload))
	}
	i.node = NodeID(payload[:idLength])
	i.value = make([]uint16, units)
	for k := range i.value {
		i.value[k] = binary.LittleEndian.Uint16(payload[int(idLength)+2*k:])
	}
	return i, nil
}

func (i appKitIntent) resolve(s *SemanticSnapshot) (AccessibilityIntent, error) {
	if s == nil {
		return AccessibilityIntent{}, ErrElementUnavailable
	}
	n, err := s.Lookup(i.node, i.revision)
	if err != nil {
		return AccessibilityIntent{}, err
	}
	intent := AccessibilityIntent{Node: i.node, Revision: i.revision, Action: i.action}
	switch i.action {
	case ActionSetValue:
		if intent.Value, err = inputtext.DecodeUTF16(i.value); err != nil {
			return AccessibilityIntent{}, fmt.Errorf("the accessibility value is not valid UTF-16; enter the text again: %w", err)
		}
		intent.Selection = inputtext.Range{Start: len(intent.Value), End: len(intent.Value)}
	case ActionSelectText:
		intent.Selection, err = appKitByteRange(n.Value, i.location, i.length)
	case ActionActivate, ActionFocus, ActionScrollIntoView, ActionLoadText:
	}
	return intent, err
}

func appKitByteRange(value string, location, length uint32) (inputtext.Range, error) {
	start, startErr := inputtext.Convert(value, int(location), inputtext.UTF16Units, inputtext.Bytes)
	end, endErr := inputtext.Convert(value, int(location)+int(length), inputtext.UTF16Units, inputtext.Bytes)
	if err := errors.Join(startErr, endErr); err != nil {
		return inputtext.Range{}, fmt.Errorf("the accessibility selection splits a character or leaves the text; select the text again: %w", err)
	}
	return inputtext.Range{Start: start, End: end}, nil
}

func serveAppKitIntents(r io.Reader, tree *AccessibilityTree, refuse func(error)) error {
	var header [appKitIntentHeader]byte
	for {
		record, err := readAppKitIntent(r, &header)
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		err = requestAppKitIntent(tree, record)
		if err != nil {
			refuse(err)
		}
	}
}

func requestAppKitIntent(tree *AccessibilityTree, record appKitIntent) error {
	s := tree.Snapshot()
	if s != nil && record.action == ActionSelectText {
		n, err := s.Lookup(record.node, record.revision)
		if err != nil {
			return err
		}
		if n.TextContainer {
			return tree.textView(s, n.ID).selectUnits(int(record.location), int(record.location)+int(record.length), inputtext.UTF16Units)
		}
	}
	intent, err := record.resolve(s)
	if err != nil {
		return err
	}
	return tree.Request(intent)
}
