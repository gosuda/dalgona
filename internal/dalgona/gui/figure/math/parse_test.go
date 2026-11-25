package math

import (
	"errors"
	"fmt"
	"reflect"
	"strings"
	"testing"
)

var spanType = reflect.TypeFor[Span]()

func stripSpans(v reflect.Value) {
	kind := v.Kind()
	if (kind == reflect.Pointer || kind == reflect.Interface) && !v.IsNil() {
		stripSpans(v.Elem())
	}
	if kind == reflect.Slice {
		for i := range v.Len() {
			stripSpans(v.Index(i))
		}
	}
	if kind == reflect.Struct {
		stripFields(v)
	}
}

func stripFields(v reflect.Value) {
	for i := range v.NumField() {
		field := v.Field(i)
		if field.Type() == spanType && field.CanSet() {
			field.Set(reflect.Zero(spanType))
			continue
		}
		stripSpans(field)
	}
}

type tree struct {
	Body []Node
	Tag  *Tag
}

func shape(formula *Formula) tree {
	shaped := tree{Body: formula.Body, Tag: formula.Tag}
	stripSpans(reflect.ValueOf(&shaped))
	return shaped
}

func mustParse(t *testing.T, source string) *Formula {
	t.Helper()
	formula, err := Parse(source)
	if err != nil {
		t.Fatalf("Parse(%q): %v", source, err)
	}
	return formula
}

func letter(r rune) *Symbol           { return &Symbol{Value: r, Class: ClassOrd} }
func sym(r rune, class Class) *Symbol { return &Symbol{Value: r, Class: class} }
func cs(name string, r rune, class Class) *Symbol {
	return &Symbol{Command: name, Value: r, Class: class}
}
func num(digits string) *Number             { return &Number{Digits: digits} }
func field(nodes ...Node) *Field            { return &Field{Body: nodes} }
func row(cells ...[]Node) Row               { return Row{Cells: cells} }
func cell(nodes ...Node) []Node             { return nodes }
func group(nodes ...Node) *Group            { return &Group{Body: nodes} }
func delim(r rune) Delimiter                { return Delimiter{Value: r} }
func largeOp(name string, r rune) *Operator { return &Operator{Command: name, Value: r} }
func fn(name string) *Operator              { return &Operator{Command: name, Name: name} }

type production struct {
	source string
	body   []Node
	tag    *Tag
}

var productions = []production{
	{`x`, cell(letter('x')), nil},
	{`3.14`, cell(num("3.14")), nil},
	{`12.`, cell(num("12"), sym('.', ClassOrd)), nil},
	{`\alpha`, cell(cs("alpha", 'α', ClassOrd)), nil},
	{`α≤β`, cell(letter('α'), sym('≤', ClassRel), letter('β')), nil},
	{`a-b+c`, cell(letter('a'), sym('−', ClassBin), letter('b'), sym('+', ClassBin), letter('c')), nil},
	{`{a}`, cell(group(letter('a'))), nil},
	{`\{\}\#\$\%\&\_\|`, cell(cs("{", '{', ClassOpen), cs("}", '}', ClassClose), cs("#", '#', ClassOrd),
		cs("$", '$', ClassOrd), cs("%", '%', ClassOrd), cs("&", '&', ClassOrd), cs("_", '_', ClassOrd),
		cs("|", '‖', ClassOrd)), nil},
	{`x % comment` + "\n" + `y`, cell(letter('x'), letter('y')), nil},
	{`x'`, cell(&Scripts{Base: letter('x'), Primes: 1}), nil},
	{`f''(x)`, cell(&Scripts{Base: letter('f'), Primes: 2}, sym('(', ClassOpen), letter('x'), sym(')', ClassClose)), nil},
	{`x^2`, cell(&Scripts{Base: letter('x'), Sup: field(num("2"))}), nil},
	{`x^23`, cell(&Scripts{Base: letter('x'), Sup: field(num("2"))}, num("3")), nil},
	{`x_i^2`, cell(&Scripts{Base: letter('x'), Sub: field(letter('i')), Sup: field(num("2"))}), nil},
	{`x^2_i`, cell(&Scripts{Base: letter('x'), Sub: field(letter('i')), Sup: field(num("2"))}), nil},
	{`x_{i_j}`, cell(&Scripts{Base: letter('x'), Sub: field(&Scripts{Base: letter('i'), Sub: field(letter('j'))})}), nil},
	{`^2x`, cell(&Scripts{Sup: field(num("2"))}, letter('x')), nil},
	{`x^\alpha`, cell(&Scripts{Base: letter('x'), Sup: field(cs("alpha", 'α', ClassOrd))}), nil},
	{`\sum_{i=1}^n`, cell(&Scripts{Base: largeOp("sum", '∑'), Sub: field(letter('i'), sym('=', ClassRel), num("1")), Sup: field(letter('n'))}), nil},
	{`\sum\limits_i`, cell(&Scripts{Base: &Operator{Command: "sum", Value: '∑', Limits: LimitsAbove}, Sub: field(letter('i'))}), nil},
	{`\int\nolimits_0^1`, cell(&Scripts{Base: &Operator{Command: "int", Value: '∫', Limits: LimitsBeside}, Sub: field(num("0")), Sup: field(num("1"))}), nil},
	{`∑_k`, cell(&Scripts{Base: &Operator{Value: '∑'}, Sub: field(letter('k'))}), nil},
	{`\displaystyle x\textstyle y\scriptstyle z\scriptscriptstyle w`, cell(&StyleChange{Command: "displaystyle"}, letter('x'),
		&StyleChange{Command: "textstyle"}, letter('y'), &StyleChange{Command: "scriptstyle"}, letter('z'),
		&StyleChange{Command: "scriptscriptstyle"}, letter('w')), nil},
	{`\frac{a}{b}`, cell(&Fraction{Command: "frac", Numerator: cell(letter('a')), Denominator: cell(letter('b'))}), nil},
	{`\dfrac12`, cell(&Fraction{Command: "dfrac", Numerator: cell(num("1")), Denominator: cell(num("2"))}), nil},
	{`\tfrac{x}{y}`, cell(&Fraction{Command: "tfrac", Numerator: cell(letter('x')), Denominator: cell(letter('y'))}), nil},
	{`\binom{n}{k}`, cell(&Fraction{Command: "binom", Numerator: cell(letter('n')), Denominator: cell(letter('k'))}), nil},
	{`\frac{1}{1+\frac{1}{x}}`, cell(&Fraction{Command: "frac", Numerator: cell(num("1")), Denominator: cell(num("1"), sym('+', ClassBin),
		&Fraction{Command: "frac", Numerator: cell(num("1")), Denominator: cell(letter('x'))})}), nil},
	{`\sqrt{x}`, cell(&Radical{Body: cell(letter('x'))}), nil},
	{`\sqrt[3]{x}`, cell(&Radical{Index: field(num("3")), Body: cell(letter('x'))}), nil},
	{`\left( x \middle| y \right)`, cell(&Fence{
		Left: delim('('), Middles: []Delimiter{delim('|')},
		Segments: [][]Node{cell(letter('x')), cell(letter('y'))}, Right: delim(')'),
	}), nil},
	{`\left. x \right\}`, cell(&Fence{Left: delim(0), Segments: [][]Node{cell(letter('x'))}, Right: Delimiter{Command: "}", Value: '}'}}), nil},
	{`\left< x \right\rangle`, cell(&Fence{Left: delim('⟨'), Segments: [][]Node{cell(letter('x'))}, Right: Delimiter{Command: "rangle", Value: '⟩'}}), nil},
	{`\bigl( x \Bigr) \bigg| \Biggm\Vert`, cell(&SizedDelimiter{Command: "bigl", Delimiter: delim('(')}, letter('x'),
		&SizedDelimiter{Command: "Bigr", Delimiter: delim(')')}, &SizedDelimiter{Command: "bigg", Delimiter: delim('|')},
		&SizedDelimiter{Command: "Biggm", Delimiter: Delimiter{Command: "Vert", Value: '‖'}}), nil},
	{`\lim_{x\to 0}`, cell(&Scripts{Base: &Operator{Command: "lim", Name: "lim"}, Sub: field(letter('x'), cs("to", '→', ClassRel), num("0"))}), nil},
	{`\limsup`, cell(&Operator{Command: "limsup", Name: "lim sup"}), nil},
	{`\operatorname{tr} A`, cell(&Operator{Command: "operatorname", Name: "tr"}, letter('A')), nil},
	{`\operatorname*{argmax}_x`, cell(&Scripts{Base: &Operator{Command: "operatorname*", Name: "argmax"}, Sub: field(letter('x'))}), nil},
	{`\sin x`, cell(fn("sin"), letter('x')), nil},
	{`\hat{x}\vec v`, cell(&Accent{Command: "hat", Body: cell(letter('x'))}, &Accent{Command: "vec", Body: cell(letter('v'))}), nil},
	{`\overline{AB}\underline{x}`, cell(&Accent{Command: "overline", Body: cell(letter('A'), letter('B'))}, &Accent{Command: "underline", Body: cell(letter('x'))}), nil},
	{`\widehat{xy}\widetilde{a}`, cell(&Accent{Command: "widehat", Body: cell(letter('x'), letter('y'))}, &Accent{Command: "widetilde", Body: cell(letter('a'))}), nil},
	{`\overbrace{a+b}^{n}`, cell(&Scripts{Base: &Accent{Command: "overbrace", Body: cell(letter('a'), sym('+', ClassBin), letter('b'))}, Sup: field(letter('n'))}), nil},
	{`\underbrace{a}_k`, cell(&Scripts{Base: &Accent{Command: "underbrace", Body: cell(letter('a'))}, Sub: field(letter('k'))}), nil},
	{`\text{if }x`, cell(&Text{Value: "if "}, letter('x')), nil},
	{`\text{a\{b\}  \$1~2}`, cell(&Text{Value: "a{b} $1\u00a02"}), nil},
	{`\mathbb{R}\mathcal L\mathbf{v}\mathit{x}\mathrm{d}\mathsf{s}\mathtt{t}`, cell(
		&Alphabet{Command: "mathbb", Body: cell(letter('R'))}, &Alphabet{Command: "mathcal", Body: cell(letter('L'))},
		&Alphabet{Command: "mathbf", Body: cell(letter('v'))}, &Alphabet{Command: "mathit", Body: cell(letter('x'))},
		&Alphabet{Command: "mathrm", Body: cell(letter('d'))}, &Alphabet{Command: "mathsf", Body: cell(letter('s'))},
		&Alphabet{Command: "mathtt", Body: cell(letter('t'))}), nil},
	{`a\quad b\qquad c\enspace d\,e\!f\:g\;h\ i~j\hspace{2em}k\hspace{-3mu}`, cell(letter('a'), &Space{Command: "quad", Amount: 18, Unit: "mu"}, letter('b'),
		&Space{Command: "qquad", Amount: 36, Unit: "mu"}, letter('c'), &Space{Command: "enspace", Amount: 9, Unit: "mu"}, letter('d'),
		&Space{Command: ",", Amount: 3, Unit: "mu"}, letter('e'), &Space{Command: "!", Amount: -3, Unit: "mu"}, letter('f'),
		&Space{Command: ":", Amount: 4, Unit: "mu"}, letter('g'), &Space{Command: ";", Amount: 5, Unit: "mu"}, letter('h'),
		&Space{Command: " ", Amount: 6, Unit: "mu"}, letter('i'), &Space{Command: "~", Amount: 6, Unit: "mu"}, letter('j'),
		&Space{Command: "hspace", Amount: 2, Unit: "em"}, letter('k'), &Space{Command: "hspace", Amount: -3, Unit: "mu"}), nil},
	{`\begin{pmatrix} a & b \\ c & d \end{pmatrix}`, cell(&Environment{Name: "pmatrix", Rows: []Row{
		row(cell(letter('a')), cell(letter('b'))), row(cell(letter('c')), cell(letter('d'))),
	}}), nil},
	{`\begin{matrix} a \\ \end{matrix}`, cell(&Environment{Name: "matrix", Rows: []Row{row(cell(letter('a')))}}), nil},
	{`\begin{smallmatrix} a \end{smallmatrix}`, cell(&Environment{Name: "smallmatrix", Rows: []Row{row(cell(letter('a')))}}), nil},
	{`\begin{array}{c|l} 1 & 2 \end{array}`, cell(&Environment{Name: "array", Columns: "c|l", Rows: []Row{row(cell(num("1")), cell(num("2")))}}), nil},
	{`\begin{alignedat}{2} a &= b & c &= d \end{alignedat}`, cell(&Environment{Name: "alignedat", Pairs: 2, Rows: []Row{
		row(cell(letter('a')), cell(sym('=', ClassRel), letter('b')), cell(letter('c')), cell(sym('=', ClassRel), letter('d'))),
	}}), nil},
	{`\begin{cases} 1 & x>0 \\ 0 & \text{otherwise} \end{cases}`, cell(&Environment{Name: "cases", Rows: []Row{
		row(cell(num("1")), cell(letter('x'), sym('>', ClassRel), num("0"))), row(cell(num("0")), cell(&Text{Value: "otherwise"})),
	}}), nil},
	{`\begin{align} a &= b \tag{1} \\ c &= d \end{align}`, cell(&Environment{Name: "align", Rows: []Row{
		{Cells: [][]Node{cell(letter('a')), cell(sym('=', ClassRel), letter('b'))}, Tag: &Tag{Text: "1"}},
		row(cell(letter('c')), cell(sym('=', ClassRel), letter('d'))),
	}}), nil},
	{`\begin{gather*} a \\ b \end{gather*}`, cell(&Environment{Name: "gather*", Rows: []Row{row(cell(letter('a'))), row(cell(letter('b')))}}), nil},
	{`\begin{equation} E \tag*{A} \end{equation}`, cell(&Environment{Name: "equation", Rows: []Row{{Cells: [][]Node{cell(letter('E'))}, Tag: &Tag{Star: true, Text: "A"}}}}), nil},
	{`x = 1 \tag*{A}`, cell(letter('x'), sym('=', ClassRel), num("1")), &Tag{Star: true, Text: "A"}},
	{
		`\begin{gathered} a \end{gathered}\begin{aligned} a \end{aligned}\begin{bmatrix}a\end{bmatrix}\begin{Bmatrix}a\end{Bmatrix}\begin{vmatrix}a\end{vmatrix}\begin{Vmatrix}a\end{Vmatrix}`,
		cell(&Environment{Name: "gathered", Rows: []Row{row(cell(letter('a')))}}, &Environment{Name: "aligned", Rows: []Row{row(cell(letter('a')))}},
			&Environment{Name: "bmatrix", Rows: []Row{row(cell(letter('a')))}}, &Environment{Name: "Bmatrix", Rows: []Row{row(cell(letter('a')))}},
			&Environment{Name: "vmatrix", Rows: []Row{row(cell(letter('a')))}}, &Environment{Name: "Vmatrix", Rows: []Row{row(cell(letter('a')))}}), nil,
	},
	{`가`, cell(letter('가')), nil},
}

func TestEveryProductionParsesToItsSemanticTree(t *testing.T) {
	for _, want := range productions {
		formula := mustParse(t, want.source)
		got := shape(formula)
		expected := tree{Body: want.body, Tag: want.tag}
		if !reflect.DeepEqual(got, expected) {
			t.Errorf("Parse(%q)\n got %s\nwant %s", want.source, describe(got), describe(expected))
		}
	}
}

func describe(v tree) string {
	var out strings.Builder
	for _, node := range v.Body {
		fmt.Fprintf(&out, "%#v ", node)
	}
	if v.Tag != nil {
		fmt.Fprintf(&out, "tag %#v", *v.Tag)
	}
	return out.String()
}

func TestEveryProductionRoundTripsThroughUnparse(t *testing.T) {
	for _, want := range productions {
		assertRoundTrip(t, want.source)
	}
}

func assertRoundTrip(t *testing.T, source string) {
	t.Helper()
	first, err := Parse(source)
	if err != nil {
		t.Fatalf("Parse(%q): %v", source, err)
	}
	text := unparse(first)
	second, err := Parse(text)
	if err != nil {
		t.Fatalf("Parse(unparse(%q)) = Parse(%q): %v", source, text, err)
	}
	if !reflect.DeepEqual(shape(first), shape(second)) {
		t.Fatalf("round trip changed %q through %q\n got %s\nwant %s", source, text, describe(shape(second)), describe(shape(first)))
	}
}

func TestSpansCoverTheirSourceBytes(t *testing.T) {
	source := `a + \frac{x^{2}}{\sqrt[3]{y}}`
	formula := mustParse(t, source)
	fraction := formula.Body[2].(*Fraction)
	if got := source[fraction.Start:fraction.End]; got != `\frac{x^{2}}{\sqrt[3]{y}}` {
		t.Fatalf("fraction span covers %q", got)
	}
	scripts := fraction.Numerator[0].(*Scripts)
	if got := source[scripts.Start:scripts.End]; got != `x^{2}` {
		t.Fatalf("scripts span covers %q", got)
	}
	radical := fraction.Denominator[0].(*Radical)
	if got := source[radical.Index.Start:radical.Index.End]; got != `[3]` {
		t.Fatalf("index span covers %q", got)
	}
}

type commandCase struct {
	name  string
	value rune
	class Class
}

var greekCases = []commandCase{
	{"alpha", 'α', ClassOrd},
	{"beta", 'β', ClassOrd},
	{"gamma", 'γ', ClassOrd},
	{"delta", 'δ', ClassOrd},
	{"epsilon", 'ϵ', ClassOrd},
	{"varepsilon", 'ε', ClassOrd},
	{"zeta", 'ζ', ClassOrd},
	{"eta", 'η', ClassOrd},
	{"theta", 'θ', ClassOrd},
	{"vartheta", 'ϑ', ClassOrd},
	{"iota", 'ι', ClassOrd},
	{"kappa", 'κ', ClassOrd},
	{"lambda", 'λ', ClassOrd},
	{"mu", 'μ', ClassOrd},
	{"nu", 'ν', ClassOrd},
	{"xi", 'ξ', ClassOrd},
	{"omicron", 'ο', ClassOrd},
	{"pi", 'π', ClassOrd},
	{"varpi", 'ϖ', ClassOrd},
	{"rho", 'ρ', ClassOrd},
	{"varrho", 'ϱ', ClassOrd},
	{"sigma", 'σ', ClassOrd},
	{"varsigma", 'ς', ClassOrd},
	{"tau", 'τ', ClassOrd},
	{"upsilon", 'υ', ClassOrd},
	{"phi", 'ϕ', ClassOrd},
	{"varphi", 'φ', ClassOrd},
	{"chi", 'χ', ClassOrd},
	{"psi", 'ψ', ClassOrd},
	{"omega", 'ω', ClassOrd},
	{"Gamma", 'Γ', ClassOrd},
	{"Delta", 'Δ', ClassOrd},
	{"Theta", 'Θ', ClassOrd},
	{"Lambda", 'Λ', ClassOrd},
	{"Xi", 'Ξ', ClassOrd},
	{"Pi", 'Π', ClassOrd},
	{"Sigma", 'Σ', ClassOrd},
	{"Upsilon", 'Υ', ClassOrd},
	{"Phi", 'Φ', ClassOrd},
	{"Psi", 'Ψ', ClassOrd},
	{"Omega", 'Ω', ClassOrd},
}

var relationCases = []commandCase{
	{"ne", '≠', ClassRel},
	{"neq", '≠', ClassRel},
	{"le", '≤', ClassRel},
	{"leq", '≤', ClassRel},
	{"ge", '≥', ClassRel},
	{"geq", '≥', ClassRel},
	{"ll", '≪', ClassRel},
	{"gg", '≫', ClassRel},
	{"approx", '≈', ClassRel},
	{"sim", '∼', ClassRel},
	{"simeq", '≃', ClassRel},
	{"cong", '≅', ClassRel},
	{"prec", '≺', ClassRel},
	{"succ", '≻', ClassRel},
	{"preceq", '⪯', ClassRel},
	{"succeq", '⪰', ClassRel},
	{"in", '∈', ClassRel},
	{"notin", '∉', ClassRel},
	{"ni", '∋', ClassRel},
	{"subset", '⊂', ClassRel},
	{"subseteq", '⊆', ClassRel},
	{"subsetneq", '⊊', ClassRel},
	{"supset", '⊃', ClassRel},
	{"supseteq", '⊇', ClassRel},
	{"supsetneq", '⊋', ClassRel},
	{"equiv", '≡', ClassRel},
}

var arrowCases = []commandCase{
	{"leftarrow", '←', ClassRel},
	{"rightarrow", '→', ClassRel},
	{"to", '→', ClassRel},
	{"gets", '←', ClassRel},
	{"leftrightarrow", '↔', ClassRel},
	{"Leftarrow", '⇐', ClassRel},
	{"Rightarrow", '⇒', ClassRel},
	{"Leftrightarrow", '⇔', ClassRel},
	{"mapsto", '↦', ClassRel},
	{"longleftarrow", '⟵', ClassRel},
	{"longrightarrow", '⟶', ClassRel},
	{"longleftrightarrow", '⟷', ClassRel},
	{"Longleftarrow", '⟸', ClassRel},
	{"Longrightarrow", '⟹', ClassRel},
	{"Longleftrightarrow", '⟺', ClassRel},
	{"longmapsto", '⟼', ClassRel},
	{"implies", '⟹', ClassRel},
	{"iff", '⟺', ClassRel},
}

var setCases = []commandCase{
	{"cup", '∪', ClassBin},
	{"cap", '∩', ClassBin},
	{"setminus", '∖', ClassBin},
	{"emptyset", '∅', ClassOrd},
	{"land", '∧', ClassBin},
	{"wedge", '∧', ClassBin},
	{"lor", '∨', ClassBin},
	{"vee", '∨', ClassBin},
	{"forall", '∀', ClassOrd},
	{"exists", '∃', ClassOrd},
	{"nexists", '∄', ClassOrd},
	{"neg", '¬', ClassOrd},
}

func TestRequiredSymbolCommandsHaveParserEntries(t *testing.T) {
	groups := map[string][]commandCase{"greek": greekCases, "relation": relationCases, "arrow": arrowCases, "set": setCases}
	for group, cases := range groups {
		for _, want := range cases {
			formula := mustParse(t, `\`+want.name)
			got, ok := formula.Body[0].(*Symbol)
			if !ok || got.Command != want.name || got.Value != want.value || got.Class != want.class {
				t.Errorf("%s \\%s parsed to %#v, want value %q class %d", group, want.name, formula.Body[0], want.value, want.class)
			}
		}
	}
}

func TestRequiredOperatorCommandsHaveParserEntries(t *testing.T) {
	large := map[string]rune{"sum": '∑', "prod": '∏', "coprod": '∐', "int": '∫', "iint": '∬', "iiint": '∭', "oint": '∮'}
	for name, value := range large {
		got, ok := mustParse(t, `\`+name).Body[0].(*Operator)
		if !ok || got.Value != value || got.Name != "" {
			t.Errorf("\\%s parsed to %#v, want large operator %q", name, got, value)
		}
	}
	named := []string{
		"sin", "cos", "tan", "cot", "sec", "csc", "sinh", "cosh", "tanh", "coth", "sech", "csch",
		"log", "ln", "exp", "det", "gcd", "Pr", "lim", "sup", "inf", "max", "min",
	}
	for _, name := range named {
		got, ok := mustParse(t, `\`+name).Body[0].(*Operator)
		if !ok || got.Name != name || got.Value != 0 {
			t.Errorf("\\%s parsed to %#v, want named function", name, got)
		}
	}
}

type refusal struct {
	source  string
	code    Code
	span    Span
	message string
}

func TestInvalidInputIsRefusedWithALocatedDiagnostic(t *testing.T) {
	refusals := []refusal{
		{`a + \foo + b`, CodeUnknownCommand, Span{4, 8}, `\foo`},
		{`\def\x{1}`, CodeForbidden, Span{0, 4}, `\def`},
		{`\newcommand{\x}{1}`, CodeForbidden, Span{0, 11}, `\newcommand`},
		{`\input{secret}`, CodeForbidden, Span{0, 6}, `\input`},
		{`x\write18{rm}`, CodeForbidden, Span{1, 7}, `\write`},
		{`\usepackage{amsmath}`, CodeForbidden, Span{0, 11}, `\usepackage`},
		{`\begin{foo} a \end{foo}`, CodeUnknownCommand, Span{6, 11}, `foo`},
		{`{x`, CodeSyntax, Span{0, 1}, `unclosed {`},
		{`x}`, CodeSyntax, Span{1, 2}, `unmatched }`},
		{`\left( x`, CodeSyntax, Span{0, 5}, `\right`},
		{`x \right)`, CodeSyntax, Span{2, 8}, `\left`},
		{`x^2^3`, CodeSyntax, Span{3, 4}, `double superscript`},
		{`x_1_2`, CodeSyntax, Span{3, 4}, `double subscript`},
		{`x^2'`, CodeSyntax, Span{3, 4}, `prime`},
		{`a & b`, CodeSyntax, Span{2, 3}, `&`},
		{`a \\ b`, CodeSyntax, Span{2, 4}, `row break`},
		{`\begin{pmatrix} a \end{bmatrix}`, CodeSyntax, Span{22, 31}, `does not match`},
		{`\begin{pmatrix} a`, CodeSyntax, Span{6, 15}, `no matching \end`},
		{`\frac{a}`, CodeSyntax, Span{0, 5}, `needs an argument`},
		{`x^`, CodeSyntax, Span{1, 2}, `needs an argument`},
		{`\text{\alpha}`, CodeUnknownCommand, Span{6, 12}, `\alpha`},
		{`\text{a^b}`, CodeSyntax, Span{7, 8}, `^`},
		{`\tag{1}\tag{2}`, CodeSyntax, Span{7, 14}, `one \tag`},
		{`{\tag{1}}`, CodeSyntax, Span{1, 8}, `top level`},
		{`\tag{1}^2`, CodeSyntax, Span{7, 8}, `scripts`},
		{`{\begin{align} a \end{align}}`, CodeSyntax, Span{7, 14}, `outermost`},
		{`\begin{equation} a \\ b \end{equation}`, CodeSyntax, Span{19, 21}, `one row`},
		{`\begin{cases} a & b & c \end{cases}`, CodeLimit, Span{20, 21}, `at most 2 columns`},
		{`\begin{array}{cx} a \end{array}`, CodeSyntax, Span{13, 17}, `l, c, r`},
		{`\begin{array}{c} a & b \end{array}`, CodeLimit, Span{19, 20}, `at most 1 columns`},
		{`\sum\limits\limits`, CodeSyntax, Span{11, 18}, `must follow`},
		{`\left x \right)`, CodeSyntax, Span{0, 5}, `delimiter`},
		{`\hspace{2}`, CodeSyntax, Span{7, 10}, `length`},
		{`\sqrt[3{x}`, CodeSyntax, Span{5, 6}, `unclosed [`},
		{`#`, CodeSyntax, Span{0, 1}, `\#`},
		{"x\x00", CodeSyntax, Span{1, 2}, `control character`},
		{"x\xff", CodeSyntax, Span{1, 2}, `UTF-8`},
		{`x\`, CodeSyntax, Span{1, 2}, `backslash`},
	}
	for _, want := range refusals {
		assertRefusal(t, want)
	}
}

func assertRefusal(t *testing.T, want refusal) {
	t.Helper()
	_, err := Parse(want.source)
	var diagnostic *Error
	if !errors.As(err, &diagnostic) {
		t.Errorf("Parse(%q) error = %v, want *Error", want.source, err)
		return
	}
	if diagnostic.Code != want.code || diagnostic.Span != want.span || !strings.Contains(diagnostic.Message, want.message) {
		t.Errorf("Parse(%q) = code %d span %v %q, want code %d span %v containing %q",
			want.source, diagnostic.Code, diagnostic.Span, diagnostic.Message, want.code, want.span, want.message)
	}
}

func matrix(rows, columns int) string {
	cells := make([]string, columns)
	for i := range cells {
		cells[i] = "x"
	}
	lines := make([]string, rows)
	for i := range lines {
		lines[i] = strings.Join(cells, "&")
	}
	return `\begin{matrix}` + strings.Join(lines, `\\`) + `\end{matrix}`
}

func TestDeclaredLimitsAcceptTheBoundAndRefuseBeyondIt(t *testing.T) {
	limits := []struct {
		name          string
		atLimit, over string
	}{
		{"source", `\text{` + strings.Repeat("x", MaxSourceBytes-7) + `}`, `\text{` + strings.Repeat("x", MaxSourceBytes-6) + `}`},
		{
			"nesting", strings.Repeat("{", MaxDepth) + "x" + strings.Repeat("}", MaxDepth),
			strings.Repeat("{", MaxDepth+1) + "x" + strings.Repeat("}", MaxDepth+1),
		},
		{"nodes", strings.Repeat("x", MaxNodes), strings.Repeat("x", MaxNodes+1)},
		{"rows", matrix(MaxRows, 1), matrix(MaxRows+1, 1)},
		{"columns", matrix(1, MaxColumns), matrix(1, MaxColumns+1)},
		{"cells", matrix(16, MaxColumns), matrix(17, 241)},
		{"space", `\hspace{10em}\hspace{-180mu}`, `\hspace{10.5em}`},
	}
	for _, limit := range limits {
		if _, err := Parse(limit.atLimit); err != nil {
			t.Errorf("%s at its limit: %v", limit.name, err)
		}
		_, err := Parse(limit.over)
		var diagnostic *Error
		if !errors.As(err, &diagnostic) || diagnostic.Code != CodeLimit {
			t.Errorf("%s beyond its limit: error %v, want a limit diagnostic", limit.name, err)
		}
	}
}

func TestDiagnosticSpanLeavesSurroundingSourceIntact(t *testing.T) {
	source := `\alpha + \unknown{x} - y`
	_, err := Parse(source)
	var diagnostic *Error
	if !errors.As(err, &diagnostic) {
		t.Fatalf("Parse(%q) = %v", source, err)
	}
	if got := source[diagnostic.Span.Start:diagnostic.Span.End]; got != `\unknown` {
		t.Fatalf("diagnostic covers %q, want only the unknown command name", got)
	}
}

func FuzzParse(f *testing.F) {
	for _, seed := range productions {
		f.Add(seed.source)
	}
	f.Add(`\left(\frac{a}{b}\right)^{2}_{\alpha'}`)
	f.Add(`\begin{array}{|c|}a\\b\end{array}`)
	f.Fuzz(func(t *testing.T, source string) {
		formula, err := Parse(source)
		if err != nil {
			var diagnostic *Error
			if !errors.As(err, &diagnostic) {
				t.Fatalf("Parse(%q) returned %T", source, err)
			}
			if diagnostic.Span.Start < 0 || diagnostic.Span.Start > diagnostic.Span.End || diagnostic.Span.End > len(source) {
				t.Fatalf("Parse(%q) diagnostic span %v is outside the source", source, diagnostic.Span)
			}
			return
		}
		if formula.Nodes > MaxNodes {
			t.Fatalf("Parse(%q) kept %d nodes", source, formula.Nodes)
		}
		assertRoundTrip(t, source)
	})
}
