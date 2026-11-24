package math

import (
	"fmt"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

const (
	MaxSourceBytes = 64 << 10
	MaxDepth       = 64
	MaxNodes       = 10_000
	MaxRows        = 256
	MaxColumns     = 256
	MaxCells       = 4_096
	maxSpaceEm     = 10
	muPerEm        = 18
	interwordMu    = 6
	maxPairs       = MaxColumns / 2
)

type Code uint8

const (
	CodeSyntax Code = iota + 1
	CodeUnknownCommand
	CodeForbidden
	CodeLimit
)

type Error struct {
	Code    Code
	Span    Span
	Message string
}

func (e *Error) Error() string {
	return fmt.Sprintf("%s (bytes %d to %d)", e.Message, e.Span.Start, e.Span.End)
}

func failure(code Code, span Span, format string, args ...any) *Error {
	return &Error{Code: code, Span: span, Message: fmt.Sprintf(format, args...)}
}

type tokenKind uint8

const (
	tokEOF tokenKind = iota
	tokChar
	tokCommand
	tokOpen
	tokClose
	tokSup
	tokSub
	tokAlign
	tokPrime
	tokTilde
)

type token struct {
	kind  tokenKind
	span  Span
	name  string
	value rune
}

var punctuationTokens = map[rune]tokenKind{
	'{': tokOpen, '}': tokClose, '^': tokSup, '_': tokSub, '&': tokAlign, '\'': tokPrime, '~': tokTilde,
}

type parser struct {
	src      string
	pos      int
	depth    int
	nodes    int
	tagSlot  **Tag
	tagDepth int
}

func Parse(source string) (*Formula, error) {
	if len(source) > MaxSourceBytes {
		return nil, failure(CodeLimit, Span{0, len(source)}, "math source has %d bytes; shorten it to at most %d bytes", len(source), MaxSourceBytes)
	}
	if !utf8.ValidString(source) {
		at := firstInvalidByte(source)
		return nil, failure(CodeSyntax, Span{at, at + 1}, "math source is not valid UTF-8; remove the invalid byte")
	}
	formula := &Formula{Source: source}
	p := &parser{src: source, tagSlot: &formula.Tag}
	body, err := p.list(ends)
	if err != nil {
		return nil, err
	}
	if err = p.expectEnd(); err != nil {
		return nil, err
	}
	formula.Body = body
	formula.Nodes = p.nodes
	return formula, nil
}

func firstInvalidByte(s string) int {
	for i, r := range s {
		if r == utf8.RuneError {
			if _, size := utf8.DecodeRuneInString(s[i:]); size == 1 {
				return i
			}
		}
	}
	return len(s)
}

func (p *parser) expectEnd() error {
	tok, err := p.peek()
	if err != nil {
		return err
	}
	if tok.kind == tokEOF {
		return nil
	}
	return p.unexpected(tok)
}

func isCommand(tok token, name string) bool {
	return tok.kind == tokCommand && tok.name == name
}

func (p *parser) unexpected(tok token) error {
	switch {
	case tok.kind == tokClose:
		return failure(CodeSyntax, tok.span, "unmatched }; remove it or add the opening {")
	case tok.kind == tokAlign:
		return failure(CodeSyntax, tok.span, "alignment marker & is outside an environment; use it inside \\begin...\\end")
	case isCommand(tok, "\\"):
		return failure(CodeSyntax, tok.span, "row break \\\\ is outside an environment; use it inside \\begin...\\end")
	case isCommand(tok, "end"):
		return failure(CodeSyntax, tok.span, "\\end has no matching \\begin")
	case isCommand(tok, "right") || isCommand(tok, "middle"):
		return failure(CodeSyntax, tok.span, "\\%s has no matching \\left", tok.name)
	}
	return failure(CodeSyntax, tok.span, "unexpected end of formula")
}

func (p *parser) peek() (token, error) {
	at := p.skipBlank(p.pos)
	if at >= len(p.src) {
		return token{kind: tokEOF, span: Span{at, at}}, nil
	}
	r, size := utf8.DecodeRuneInString(p.src[at:])
	if r == '\\' {
		return p.lexCommand(at)
	}
	if kind, ok := punctuationTokens[r]; ok {
		return token{kind: kind, span: Span{at, at + size}, value: r}, nil
	}
	if unicode.IsControl(r) {
		return token{}, failure(CodeSyntax, Span{at, at + size}, "control character U+%04X is not allowed in a formula", r)
	}
	return token{kind: tokChar, span: Span{at, at + size}, value: r}, nil
}

func (p *parser) skipBlank(at int) int {
	for at < len(p.src) {
		switch p.src[at] {
		case ' ', '\t', '\n', '\r':
			at++
		case '%':
			end := strings.IndexByte(p.src[at:], '\n')
			if end < 0 {
				return len(p.src)
			}
			at += end + 1
		default:
			return at
		}
	}
	return at
}

func (p *parser) lexCommand(at int) (token, error) {
	end := at + 1
	for end < len(p.src) && isASCIILetter(p.src[end]) {
		end++
	}
	if end > at+1 {
		return token{kind: tokCommand, span: Span{at, end}, name: p.src[at+1 : end]}, nil
	}
	if end >= len(p.src) {
		return token{}, failure(CodeSyntax, Span{at, end}, "formula ends with a backslash; write \\backslash for a literal backslash")
	}
	_, size := utf8.DecodeRuneInString(p.src[end:])
	return token{kind: tokCommand, span: Span{at, end + size}, name: p.src[end : end+size]}, nil
}

func isASCIILetter(b byte) bool {
	return 'a' <= b && b <= 'z' || 'A' <= b && b <= 'Z'
}

func isDigitRune(r rune) bool {
	return '0' <= r && r <= '9'
}

func isDigit(b byte) bool {
	return '0' <= b && b <= '9'
}

func (p *parser) advance(tok token) {
	p.pos = tok.span.End
}

func (p *parser) count(span Span) error {
	p.nodes++
	if p.nodes > MaxNodes {
		return failure(CodeLimit, span, "formula has more than %d nodes; split it into smaller formulas", MaxNodes)
	}
	return nil
}

func (p *parser) enter(span Span) error {
	p.depth++
	if p.depth > MaxDepth {
		return failure(CodeLimit, span, "formula nests deeper than %d levels; reduce the nesting", MaxDepth)
	}
	return nil
}

func (p *parser) leave() {
	p.depth--
}

func ends(tok token) bool {
	switch tok.kind {
	case tokEOF, tokClose, tokAlign:
		return true
	case tokCommand:
		switch tok.name {
		case "\\", "end", "right", "middle":
			return true
		}
	case tokChar, tokOpen, tokSup, tokSub, tokPrime, tokTilde:
	}
	return false
}

func endsIndex(tok token) bool {
	return ends(tok) || tok.kind == tokChar && tok.value == ']'
}

func (p *parser) list(stop func(token) bool) ([]Node, error) {
	var items []Node
	for {
		tok, err := p.peek()
		if err != nil {
			return nil, err
		}
		if stop(tok) {
			return items, nil
		}
		item, err := p.item(tok)
		if err != nil {
			return nil, err
		}
		if item != nil {
			items = append(items, item)
		}
	}
}

func isScriptToken(tok token) bool {
	return tok.kind == tokSup || tok.kind == tokSub || tok.kind == tokPrime
}

func (p *parser) item(tok token) (Node, error) {
	if isScriptToken(tok) {
		return p.scripts(nil, tok.span.Start)
	}
	base, err := p.atom(tok)
	if err != nil {
		return nil, err
	}
	switch base.(type) {
	case nil:
		return nil, p.rejectScripts(tok)
	case *Space, *StyleChange:
		return base, nil
	}
	return p.scripts(base, tok.span.Start)
}

func (p *parser) rejectScripts(owner token) error {
	tok, err := p.peek()
	if err != nil || !isScriptToken(tok) {
		return err
	}
	return failure(CodeSyntax, tok.span, "\\%s cannot take scripts; attach them to a symbol", owner.name)
}

func (p *parser) scripts(base Node, start int) (Node, error) {
	node := &Scripts{Base: base}
	for {
		tok, err := p.peek()
		if err != nil {
			return nil, err
		}
		if !isScriptToken(tok) {
			break
		}
		if err = p.script(node, tok); err != nil {
			return nil, err
		}
	}
	if node.Primes == 0 && node.Sub == nil && node.Sup == nil {
		return base, nil
	}
	node.Span = Span{start, p.pos}
	return node, p.count(node.Span)
}

func (p *parser) script(node *Scripts, tok token) error {
	p.advance(tok)
	if tok.kind == tokPrime {
		return p.prime(node, tok)
	}
	slot, name := &node.Sub, "subscript"
	if tok.kind == tokSup {
		slot, name = &node.Sup, "superscript"
	}
	if *slot != nil {
		return failure(CodeSyntax, tok.span, "double %s; group the %ss in braces", name, name)
	}
	field, err := p.argument(tok, string(tok.value))
	if err != nil {
		return err
	}
	*slot = field
	return nil
}

func (p *parser) prime(node *Scripts, tok token) error {
	if node.Sup != nil {
		return failure(CodeSyntax, tok.span, "prime follows a superscript; write the primes before ^")
	}
	node.Primes++
	return p.count(tok.span)
}

func (p *parser) argument(owner token, label string) (*Field, error) {
	tok, err := p.peek()
	if err != nil {
		return nil, err
	}
	if tok.kind == tokOpen {
		return p.bracedField(tok)
	}
	if ends(tok) || isScriptToken(tok) {
		return nil, failure(CodeSyntax, owner.span, "%s needs an argument; add {...} after it", label)
	}
	node, err := p.singleAtom(tok)
	if err != nil {
		return nil, err
	}
	if node == nil {
		return nil, failure(CodeSyntax, tok.span, "\\tag cannot be an argument; place it at the top level of the formula or row")
	}
	field := &Field{Span: Span{tok.span.Start, p.pos}, Body: []Node{node}}
	return field, p.count(field.Span)
}

func (p *parser) singleAtom(tok token) (Node, error) {
	if tok.kind != tokChar || !isDigitRune(tok.value) {
		return p.atom(tok)
	}
	p.advance(tok)
	node := &Number{Span: tok.span, Digits: string(tok.value)}
	return node, p.count(node.Span)
}

func (p *parser) bracedField(open token) (*Field, error) {
	body, err := p.braced(open)
	if err != nil {
		return nil, err
	}
	field := &Field{Span: Span{open.span.Start, p.pos}, Body: body}
	return field, p.count(field.Span)
}

func (p *parser) braced(open token) ([]Node, error) {
	p.advance(open)
	if err := p.enter(open.span); err != nil {
		return nil, err
	}
	body, err := p.list(ends)
	if err != nil {
		return nil, err
	}
	closing, err := p.peek()
	if err != nil {
		return nil, err
	}
	if closing.kind != tokClose {
		if closing.kind == tokEOF {
			return nil, failure(CodeSyntax, open.span, "unclosed {; add the matching }")
		}
		return nil, p.unexpected(closing)
	}
	p.advance(closing)
	p.leave()
	return body, nil
}

func (p *parser) atom(tok token) (Node, error) {
	switch tok.kind {
	case tokOpen:
		body, err := p.braced(tok)
		if err != nil {
			return nil, err
		}
		group := &Group{Span: Span{tok.span.Start, p.pos}, Body: body}
		return group, p.count(group.Span)
	case tokChar:
		return p.character(tok)
	case tokTilde:
		p.advance(tok)
		node := &Space{Span: tok.span, Command: "~", Amount: interwordMu, Unit: "mu"}
		return node, p.count(node.Span)
	case tokCommand:
		return p.command(tok)
	case tokEOF, tokClose, tokSup, tokSub, tokAlign, tokPrime:
	}
	return nil, p.unexpected(tok)
}

func (p *parser) character(tok token) (Node, error) {
	p.advance(tok)
	r := tok.value
	if isDigitRune(r) {
		return p.number(tok)
	}
	if r == '#' || r == '$' {
		return nil, failure(CodeSyntax, tok.span, "unescaped %c; write \\%c for a literal %c", r, r, r)
	}
	entry, ok := asciiLiterals[r]
	if !ok {
		entry, ok = unicodeLiterals[r]
	}
	if !ok {
		entry = literal{value: r, class: ClassOrd}
	}
	var node Node = &Symbol{Span: tok.span, Value: entry.value, Class: entry.class}
	if entry.large {
		operator := &Operator{Span: tok.span, Value: entry.value}
		if err := p.limits(operator); err != nil {
			return nil, err
		}
		node = operator
	}
	return node, p.count(tok.span)
}

func (p *parser) number(tok token) (Node, error) {
	end := tok.span.End
	for end < len(p.src) {
		if isDigit(p.src[end]) {
			end++
			continue
		}
		if p.src[end] == '.' && end+1 < len(p.src) && isDigit(p.src[end+1]) {
			end += 2
			continue
		}
		break
	}
	p.pos = end
	node := &Number{Span: Span{tok.span.Start, end}, Digits: p.src[tok.span.Start:end]}
	return node, p.count(node.Span)
}

func (p *parser) limits(operator *Operator) error {
	tok, err := p.peek()
	if err != nil || tok.kind != tokCommand {
		return err
	}
	switch tok.name {
	case "limits":
		operator.Limits = LimitsAbove
	case "nolimits":
		operator.Limits = LimitsBeside
	default:
		return nil
	}
	p.advance(tok)
	operator.End = p.pos
	return nil
}

type commandHandler func(p *parser, tok token, entry command) (Node, error)

var commandHandlers map[commandKind]commandHandler

func init() {
	commandHandlers = map[commandKind]commandHandler{
		kindSymbol:        (*parser).symbolCommand,
		kindLargeOperator: (*parser).operatorCommand,
		kindFunction:      (*parser).operatorCommand,
		kindFraction:      (*parser).fraction,
		kindRadical:       (*parser).radical,
		kindLeft:          (*parser).fence,
		kindMiddle:        (*parser).unmatched,
		kindRight:         (*parser).unmatched,
		kindSized:         (*parser).sizedDelimiter,
		kindAccent:        (*parser).accent,
		kindAlphabet:      (*parser).alphabet,
		kindText:          (*parser).text,
		kindOperatorName:  (*parser).operatorName,
		kindSpace:         (*parser).space,
		kindExplicitSpace: (*parser).explicitSpace,
		kindStyle:         (*parser).style,
		kindBegin:         (*parser).environment,
		kindEnd:           (*parser).unmatched,
		kindTag:           (*parser).tag,
		kindLimits:        (*parser).misplacedLimits,
		kindForbidden:     (*parser).forbiddenCommand,
	}
}

func (p *parser) command(tok token) (Node, error) {
	entry, ok := commandTable[tok.name]
	if !ok {
		if tok.name == "\\" {
			return nil, p.unexpected(tok)
		}
		return nil, failure(CodeUnknownCommand, tok.span, "unknown command \\%s; remove it or use a supported command", tok.name)
	}
	p.advance(tok)
	return commandHandlers[entry.kind](p, tok, entry)
}

func (p *parser) symbolCommand(tok token, entry command) (Node, error) {
	node := &Symbol{Span: tok.span, Command: tok.name, Value: entry.value, Class: entry.class}
	return node, p.count(node.Span)
}

func (p *parser) operatorCommand(tok token, entry command) (Node, error) {
	node := &Operator{Span: tok.span, Command: tok.name, Name: entry.name, Value: entry.value}
	if err := p.limits(node); err != nil {
		return nil, err
	}
	return node, p.count(node.Span)
}

func (p *parser) unmatched(tok token, _ command) (Node, error) {
	return nil, p.unexpected(tok)
}

func (p *parser) misplacedLimits(tok token, _ command) (Node, error) {
	return nil, failure(CodeSyntax, tok.span, "\\%s must follow a large operator or function such as \\sum or \\lim", tok.name)
}

func (p *parser) forbiddenCommand(tok token, entry command) (Node, error) {
	return nil, failure(CodeForbidden, tok.span, "\\%s %s", tok.name, entry.name)
}

func (p *parser) fraction(tok token, _ command) (Node, error) {
	label := "\\" + tok.name
	numerator, err := p.argument(tok, label)
	if err != nil {
		return nil, err
	}
	denominator, err := p.argument(tok, label)
	if err != nil {
		return nil, err
	}
	node := &Fraction{Span: Span{tok.span.Start, p.pos}, Command: tok.name, Numerator: numerator.Body, Denominator: denominator.Body}
	return node, p.count(node.Span)
}

func (p *parser) radical(tok token, _ command) (Node, error) {
	index, err := p.radicalIndex()
	if err != nil {
		return nil, err
	}
	body, err := p.argument(tok, "\\sqrt")
	if err != nil {
		return nil, err
	}
	node := &Radical{Span: Span{tok.span.Start, p.pos}, Index: index, Body: body.Body}
	return node, p.count(node.Span)
}

func (p *parser) radicalIndex() (*Field, error) {
	open, err := p.peek()
	if err != nil || open.kind != tokChar || open.value != '[' {
		return nil, err
	}
	p.advance(open)
	if err = p.enter(open.span); err != nil {
		return nil, err
	}
	body, err := p.list(endsIndex)
	if err != nil {
		return nil, err
	}
	closing, err := p.peek()
	if err != nil {
		return nil, err
	}
	if closing.kind != tokChar || closing.value != ']' {
		return nil, failure(CodeSyntax, open.span, "unclosed [ in \\sqrt; add the matching ]")
	}
	p.advance(closing)
	p.leave()
	field := &Field{Span: Span{open.span.Start, p.pos}, Body: body}
	return field, p.count(field.Span)
}

func (p *parser) delimiter(owner token) (Delimiter, error) {
	tok, err := p.peek()
	if err != nil {
		return Delimiter{}, err
	}
	delimiter, ok := delimiterToken(tok)
	if !ok {
		return Delimiter{}, failure(CodeSyntax, owner.span, "\\%s needs a delimiter such as ( [ \\{ | or .", owner.name)
	}
	p.advance(tok)
	return delimiter, p.count(tok.span)
}

var delimiterAliases = map[rune]rune{'.': 0, '<': '⟨', '>': '⟩'}

func delimiterToken(tok token) (Delimiter, bool) {
	if tok.kind == tokCommand {
		entry, ok := commandTable[tok.name]
		ok = ok && entry.kind == kindSymbol && delimiterRunes[entry.value]
		return Delimiter{Span: tok.span, Command: tok.name, Value: entry.value}, ok
	}
	if tok.kind != tokChar {
		return Delimiter{}, false
	}
	if alias, ok := delimiterAliases[tok.value]; ok {
		return Delimiter{Span: tok.span, Value: alias}, true
	}
	return Delimiter{Span: tok.span, Value: tok.value}, delimiterRunes[tok.value]
}

func (p *parser) fence(tok token, _ command) (Node, error) {
	left, err := p.delimiter(tok)
	if err != nil {
		return nil, err
	}
	if err = p.enter(tok.span); err != nil {
		return nil, err
	}
	node := &Fence{Left: left}
	for closed := false; !closed; {
		if closed, err = p.fenceSegment(node, tok); err != nil {
			return nil, err
		}
	}
	p.leave()
	node.Span = Span{tok.span.Start, p.pos}
	return node, p.count(node.Span)
}

func (p *parser) fenceSegment(node *Fence, left token) (bool, error) {
	body, err := p.list(ends)
	if err != nil {
		return false, err
	}
	node.Segments = append(node.Segments, body)
	closing, err := p.peek()
	if err != nil {
		return false, err
	}
	if !isCommand(closing, "middle") && !isCommand(closing, "right") {
		return false, failure(CodeSyntax, left.span, "\\left has no matching \\right; add \\right before the group or formula ends")
	}
	p.advance(closing)
	delimiter, err := p.delimiter(closing)
	if err != nil {
		return false, err
	}
	if closing.name == "right" {
		node.Right = delimiter
		return true, nil
	}
	node.Middles = append(node.Middles, delimiter)
	return false, nil
}

func (p *parser) sizedDelimiter(tok token, _ command) (Node, error) {
	delimiter, err := p.delimiter(tok)
	if err != nil {
		return nil, err
	}
	node := &SizedDelimiter{Span: Span{tok.span.Start, p.pos}, Command: tok.name, Delimiter: delimiter}
	return node, p.count(node.Span)
}

func (p *parser) accent(tok token, _ command) (Node, error) {
	body, err := p.argument(tok, "\\"+tok.name)
	if err != nil {
		return nil, err
	}
	node := &Accent{Span: Span{tok.span.Start, p.pos}, Command: tok.name, Body: body.Body}
	return node, p.count(node.Span)
}

func (p *parser) alphabet(tok token, _ command) (Node, error) {
	body, err := p.argument(tok, "\\"+tok.name)
	if err != nil {
		return nil, err
	}
	node := &Alphabet{Span: Span{tok.span.Start, p.pos}, Command: tok.name, Body: body.Body}
	return node, p.count(node.Span)
}

func (p *parser) text(tok token, _ command) (Node, error) {
	value, err := p.textArgument(tok)
	if err != nil {
		return nil, err
	}
	node := &Text{Span: Span{tok.span.Start, p.pos}, Value: value}
	return node, p.count(node.Span)
}

func (p *parser) star() bool {
	if p.pos < len(p.src) && p.src[p.pos] == '*' {
		p.pos++
		return true
	}
	return false
}

func (p *parser) operatorName(tok token, _ command) (Node, error) {
	name := tok.name
	if p.star() {
		name += "*"
	}
	value, err := p.textArgument(tok)
	if err != nil {
		return nil, err
	}
	if strings.TrimSpace(value) == "" {
		return nil, failure(CodeSyntax, tok.span, "\\operatorname needs a nonempty name")
	}
	node := &Operator{Span: Span{tok.span.Start, p.pos}, Command: name, Name: value}
	if err = p.limits(node); err != nil {
		return nil, err
	}
	return node, p.count(node.Span)
}

func (p *parser) space(tok token, entry command) (Node, error) {
	node := &Space{Span: tok.span, Command: tok.name, Amount: entry.amount, Unit: "mu"}
	return node, p.count(node.Span)
}

func (p *parser) explicitSpace(tok token, _ command) (Node, error) {
	raw, span, err := p.rawArgument(tok)
	if err != nil {
		return nil, err
	}
	amount, unit, ok := parseLength(raw)
	if !ok {
		return nil, failure(CodeSyntax, span, "\\hspace needs a length such as 1em or -3mu")
	}
	if limit := spaceInEm(amount, unit); limit > maxSpaceEm || limit < -maxSpaceEm {
		return nil, failure(CodeLimit, span, "\\hspace{%s} exceeds %dem; use a smaller space", raw, maxSpaceEm)
	}
	node := &Space{Span: Span{tok.span.Start, p.pos}, Command: tok.name, Amount: amount, Unit: unit}
	return node, p.count(node.Span)
}

func spaceInEm(amount float32, unit string) float32 {
	if unit == "mu" {
		return amount / muPerEm
	}
	return amount
}

func parseLength(raw string) (float32, string, bool) {
	trimmed := strings.TrimSpace(raw)
	if len(trimmed) < 2 {
		return 0, "", false
	}
	unit := trimmed[len(trimmed)-2:]
	if unit != "em" && unit != "mu" {
		return 0, "", false
	}
	number := strings.TrimSpace(trimmed[:len(trimmed)-2])
	if !isDecimal(number) {
		return 0, "", false
	}
	value, err := strconv.ParseFloat(number, 32)
	if err != nil {
		return 0, "", false
	}
	return float32(value), unit, true
}

func isDecimal(s string) bool {
	s = strings.TrimPrefix(s, "-")
	digits, dots := 0, 0
	for i := range len(s) {
		switch {
		case isDigit(s[i]):
			digits++
		case s[i] == '.':
			dots++
		default:
			return false
		}
	}
	return digits > 0 && dots <= 1
}

func (p *parser) style(tok token, _ command) (Node, error) {
	node := &StyleChange{Span: tok.span, Command: tok.name}
	return node, p.count(node.Span)
}

func (p *parser) tag(tok token, _ command) (Node, error) {
	star := p.star()
	value, err := p.textArgument(tok)
	if err != nil {
		return nil, err
	}
	span := Span{tok.span.Start, p.pos}
	if p.tagSlot == nil || p.depth != p.tagDepth {
		return nil, failure(CodeSyntax, span, "\\tag must stand at the top level of a formula or of an align, gather, or equation row")
	}
	if *p.tagSlot != nil {
		return nil, failure(CodeSyntax, span, "a formula or row has one \\tag; remove the extra tag")
	}
	*p.tagSlot = &Tag{Span: span, Star: star, Text: value}
	return nil, p.count(span)
}

func (p *parser) rawArgument(owner token) (string, Span, error) {
	open, err := p.peek()
	if err != nil {
		return "", Span{}, err
	}
	if open.kind != tokOpen {
		return "", owner.span, failure(CodeSyntax, owner.span, "\\%s needs a braced argument", owner.name)
	}
	end := strings.IndexByte(p.src[open.span.End:], '}')
	if end < 0 {
		return "", open.span, failure(CodeSyntax, open.span, "unclosed {; add the matching }")
	}
	start := open.span.End
	p.pos = start + end + 1
	return p.src[start : start+end], Span{open.span.Start, p.pos}, nil
}

var textEscapes = map[byte]rune{'{': '{', '}': '}', '$': '$', '%': '%', '&': '&', '#': '#', '_': '_', ' ': ' '}

func (p *parser) textArgument(owner token) (string, error) {
	open, err := p.peek()
	if err != nil {
		return "", err
	}
	if open.kind != tokOpen {
		return "", failure(CodeSyntax, owner.span, "\\%s needs a braced argument", owner.name)
	}
	scan := textScan{src: p.src, at: open.span.End}
	value, err := scan.run(open.span)
	if err != nil {
		return "", err
	}
	p.pos = scan.at
	return value, nil
}

type textScan struct {
	src   string
	at    int
	depth int
	out   strings.Builder
	blank bool
}

func (s *textScan) run(open Span) (string, error) {
	for s.at < len(s.src) {
		done, err := s.step()
		if err != nil || done {
			return s.out.String(), err
		}
	}
	return "", failure(CodeSyntax, open, "unclosed {; add the matching }")
}

func (s *textScan) step() (bool, error) {
	r, size := utf8.DecodeRuneInString(s.src[s.at:])
	span := Span{s.at, s.at + size}
	s.at += size
	switch r {
	case '{':
		s.depth++
	case '}':
		if s.depth == 0 {
			return true, nil
		}
		s.depth--
	case '\\':
		return false, s.escape(span)
	case ' ', '\t', '\n', '\r':
		s.write(' ')
	case '~':
		s.write('\u00a0')
	case '$', '%', '&', '#', '^', '_':
		return false, failure(CodeSyntax, span, "unescaped %c inside text; write \\%c", r, r)
	default:
		return false, s.literal(r, span)
	}
	return false, nil
}

func (s *textScan) literal(r rune, span Span) error {
	if unicode.IsControl(r) {
		return failure(CodeSyntax, span, "control character U+%04X is not allowed in a formula", r)
	}
	s.write(r)
	return nil
}

func (s *textScan) escape(span Span) error {
	if s.at >= len(s.src) {
		return failure(CodeSyntax, span, "formula ends with a backslash; write \\backslash for a literal backslash")
	}
	r, ok := textEscapes[s.src[s.at]]
	if !ok {
		end := s.at
		for end < len(s.src) && isASCIILetter(s.src[end]) {
			end++
		}
		if end == s.at {
			_, size := utf8.DecodeRuneInString(s.src[s.at:])
			end += size
		}
		return failure(CodeUnknownCommand, Span{span.Start, end}, "command \\%s is not supported inside text", s.src[s.at:end])
	}
	s.at++
	s.write(r)
	return nil
}

func (s *textScan) write(r rune) {
	if r == ' ' {
		if s.blank {
			return
		}
		s.blank = true
	} else {
		s.blank = false
	}
	s.out.WriteRune(r)
}

func (p *parser) environment(tok token, _ command) (Node, error) {
	name, nameSpan, err := p.environmentName(tok)
	if err != nil {
		return nil, err
	}
	kind, ok := environments[name]
	if !ok {
		return nil, failure(CodeUnknownCommand, nameSpan, "unknown environment %s; use a supported environment such as aligned or pmatrix", name)
	}
	if kind.topLevel && p.depth != 0 {
		return nil, failure(CodeSyntax, nameSpan, "%s must be the outermost structure of a formula; use aligned or gathered inside other content", name)
	}
	env := &Environment{Name: name}
	if err = p.environmentArguments(tok, env, kind); err != nil {
		return nil, err
	}
	if err = p.enter(tok.span); err != nil {
		return nil, err
	}
	savedSlot, savedDepth := p.tagSlot, p.tagDepth
	err = p.rows(env, kind, nameSpan)
	p.tagSlot, p.tagDepth = savedSlot, savedDepth
	if err != nil {
		return nil, err
	}
	p.leave()
	env.Span = Span{tok.span.Start, p.pos}
	return env, p.count(env.Span)
}

func (p *parser) environmentName(owner token) (string, Span, error) {
	raw, span, err := p.rawArgument(owner)
	if err != nil {
		return "", span, err
	}
	for i := range len(raw) {
		if !isASCIILetter(raw[i]) && raw[i] != '*' {
			return "", span, failure(CodeSyntax, span, "environment name %q is not valid", raw)
		}
	}
	return raw, span, nil
}

func (p *parser) environmentArguments(tok token, env *Environment, kind environmentKind) error {
	if kind.arraySpec {
		raw, span, err := p.rawArgument(tok)
		if err != nil {
			return err
		}
		env.Columns, err = columnSpec(raw, span)
		if err != nil {
			return err
		}
	}
	if kind.pairsArg {
		raw, span, err := p.rawArgument(tok)
		if err != nil {
			return err
		}
		pairs, err := strconv.Atoi(strings.TrimSpace(raw))
		if err != nil || pairs < 1 || pairs > maxPairs {
			return failure(CodeSyntax, span, "alignedat needs a column-pair count from 1 to %d", maxPairs)
		}
		env.Pairs = pairs
	}
	return nil
}

func columnSpec(raw string, span Span) (string, error) {
	var spec strings.Builder
	columns := 0
	for i := range len(raw) {
		switch raw[i] {
		case 'l', 'c', 'r':
			columns++
			spec.WriteByte(raw[i])
		case '|':
			spec.WriteByte('|')
		case ' ':
		default:
			return "", failure(CodeSyntax, span, "array column specification %q accepts only l, c, r, and |", raw)
		}
	}
	if columns == 0 || columns > MaxColumns {
		return "", failure(CodeLimit, span, "array needs from 1 to %d columns", MaxColumns)
	}
	return spec.String(), nil
}

func (env *Environment) columnLimit(kind environmentKind) int {
	switch {
	case kind.arraySpec:
		return strings.Count(env.Columns, "l") + strings.Count(env.Columns, "c") + strings.Count(env.Columns, "r")
	case kind.pairsArg:
		return 2 * env.Pairs
	case kind.columns > 0:
		return kind.columns
	}
	return MaxColumns
}

type rowScan struct {
	env   *Environment
	kind  environmentKind
	limit int
	cells int
	name  Span
}

func (p *parser) rows(env *Environment, kind environmentKind, name Span) error {
	scan := rowScan{env: env, kind: kind, limit: env.columnLimit(kind), name: name}
	for {
		row := new(Row)
		p.tagSlot, p.tagDepth = nil, p.depth
		if kind.tags {
			p.tagSlot = &row.Tag
		}
		done, err := p.row(&scan, row)
		if err != nil {
			return err
		}
		if done {
			return nil
		}
	}
}

func (p *parser) row(scan *rowScan, row *Row) (bool, error) {
	for {
		body, err := p.list(ends)
		if err != nil {
			return false, err
		}
		row.Cells = append(row.Cells, body)
		scan.cells++
		tok, err := p.peek()
		if err != nil {
			return false, err
		}
		if err = scan.check(row, tok); err != nil {
			return false, err
		}
		p.advance(tok)
		switch {
		case tok.kind == tokAlign:
			continue
		case isCommand(tok, "\\"):
			return false, scan.append(row, tok)
		case isCommand(tok, "end"):
			return true, p.finish(scan, row, tok)
		}
		return false, p.unexpected(tok)
	}
}

func (s *rowScan) check(row *Row, tok token) error {
	switch {
	case s.cells > MaxCells:
		return failure(CodeLimit, tok.span, "%s has more than %d cells", s.env.Name, MaxCells)
	case tok.kind == tokAlign && len(row.Cells) >= s.limit:
		return failure(CodeLimit, tok.span, "%s allows at most %d columns per row", s.env.Name, s.limit)
	case isCommand(tok, "\\") && s.kind.singleRow:
		return failure(CodeSyntax, tok.span, "%s has one row; use align or gather for several rows", s.env.Name)
	case tok.kind == tokEOF:
		return failure(CodeSyntax, s.name, "\\begin{%s} has no matching \\end{%s}", s.env.Name, s.env.Name)
	}
	return nil
}

func (s *rowScan) append(row *Row, tok token) error {
	s.env.Rows = append(s.env.Rows, *row)
	if len(s.env.Rows) > MaxRows {
		return failure(CodeLimit, tok.span, "%s has more than %d rows", s.env.Name, MaxRows)
	}
	return nil
}

func (p *parser) finish(scan *rowScan, row *Row, end token) error {
	name, span, err := p.environmentName(end)
	if err != nil {
		return err
	}
	if name != scan.env.Name {
		return failure(CodeSyntax, span, "\\end{%s} does not match \\begin{%s}", name, scan.env.Name)
	}
	trailing := len(scan.env.Rows) > 0 && len(row.Cells) == 1 && len(row.Cells[0]) == 0 && row.Tag == nil
	if trailing {
		return nil
	}
	return scan.append(row, end)
}
