package math

type commandKind uint8

const (
	kindSymbol commandKind = iota
	kindLargeOperator
	kindFunction
	kindFraction
	kindRadical
	kindLeft
	kindMiddle
	kindRight
	kindSized
	kindAccent
	kindAlphabet
	kindText
	kindOperatorName
	kindSpace
	kindExplicitSpace
	kindStyle
	kindBegin
	kindEnd
	kindTag
	kindLimits
	kindForbidden
)

type accentForm uint8

const (
	accentGlyph accentForm = iota
	accentWide
	accentOverline
	accentUnderline
	accentOverBrace
	accentUnderBrace
)

type command struct {
	kind   commandKind
	value  rune
	class  Class
	limits bool
	amount float32
	name   string
	form   accentForm
}

func symbol(value rune, class Class) command {
	return command{kind: kindSymbol, value: value, class: class}
}

func largeOperator(value rune, limits bool) command {
	return command{kind: kindLargeOperator, value: value, class: ClassOp, limits: limits}
}

func function(name string, limits bool) command {
	return command{kind: kindFunction, class: ClassOp, limits: limits, name: name}
}

func sized(amount float32, class Class) command {
	return command{kind: kindSized, class: class, amount: amount}
}

func accent(value rune, form accentForm) command {
	return command{kind: kindAccent, value: value, form: form}
}

func space(mu float32) command {
	return command{kind: kindSpace, amount: mu}
}

func forbidden(reason string) command {
	return command{kind: kindForbidden, name: reason}
}

const (
	reasonDefinition = "defines a command, and formulas cannot define commands"
	reasonFile       = "reads or writes files or packages, and formulas cannot access them"
	reasonProgram    = "runs TeX programs, and formulas accept only the declared math grammar"
)

var greekCommands = ordinarySymbols(map[string]rune{
	"alpha": 'α', "beta": 'β', "gamma": 'γ', "delta": 'δ', "epsilon": 'ϵ', "varepsilon": 'ε',
	"zeta": 'ζ', "eta": 'η', "theta": 'θ', "vartheta": 'ϑ', "iota": 'ι', "kappa": 'κ',
	"varkappa": 'ϰ', "lambda": 'λ', "mu": 'μ', "nu": 'ν', "xi": 'ξ', "omicron": 'ο',
	"pi": 'π', "varpi": 'ϖ', "rho": 'ρ', "varrho": 'ϱ', "sigma": 'σ', "varsigma": 'ς',
	"tau": 'τ', "upsilon": 'υ', "phi": 'ϕ', "varphi": 'φ', "chi": 'χ', "psi": 'ψ', "omega": 'ω',
	"Gamma": 'Γ', "Delta": 'Δ', "Theta": 'Θ', "Lambda": 'Λ', "Xi": 'Ξ', "Pi": 'Π',
	"Sigma": 'Σ', "Upsilon": 'Υ', "Phi": 'Φ', "Psi": 'Ψ', "Omega": 'Ω',
})

func ordinarySymbols(runes map[string]rune) map[string]command {
	commands := make(map[string]command, len(runes))
	for name, r := range runes {
		commands[name] = symbol(r, ClassOrd)
	}
	return commands
}

var relationCommands = map[string]command{
	"ne": symbol('≠', ClassRel), "neq": symbol('≠', ClassRel),
	"le": symbol('≤', ClassRel), "leq": symbol('≤', ClassRel), "ge": symbol('≥', ClassRel), "geq": symbol('≥', ClassRel),
	"leqslant": symbol('⩽', ClassRel), "geqslant": symbol('⩾', ClassRel),
	"ll": symbol('≪', ClassRel), "gg": symbol('≫', ClassRel),
	"approx": symbol('≈', ClassRel), "sim": symbol('∼', ClassRel), "simeq": symbol('≃', ClassRel),
	"cong": symbol('≅', ClassRel), "asymp": symbol('≍', ClassRel), "doteq": symbol('≐', ClassRel),
	"equiv": symbol('≡', ClassRel), "propto": symbol('∝', ClassRel),
	"prec": symbol('≺', ClassRel), "succ": symbol('≻', ClassRel), "preceq": symbol('⪯', ClassRel), "succeq": symbol('⪰', ClassRel),
	"in": symbol('∈', ClassRel), "notin": symbol('∉', ClassRel), "ni": symbol('∋', ClassRel),
	"subset": symbol('⊂', ClassRel), "subseteq": symbol('⊆', ClassRel), "subsetneq": symbol('⊊', ClassRel),
	"supset": symbol('⊃', ClassRel), "supseteq": symbol('⊇', ClassRel), "supsetneq": symbol('⊋', ClassRel),
	"mid": symbol('∣', ClassRel), "parallel": symbol('∥', ClassRel), "perp": symbol('⟂', ClassRel),
	"models": symbol('⊨', ClassRel),
}

var arrowCommands = map[string]command{
	"leftarrow": symbol('←', ClassRel), "gets": symbol('←', ClassRel),
	"rightarrow": symbol('→', ClassRel), "to": symbol('→', ClassRel),
	"leftrightarrow": symbol('↔', ClassRel),
	"Leftarrow":      symbol('⇐', ClassRel), "Rightarrow": symbol('⇒', ClassRel), "Leftrightarrow": symbol('⇔', ClassRel),
	"mapsto":        symbol('↦', ClassRel),
	"longleftarrow": symbol('⟵', ClassRel), "longrightarrow": symbol('⟶', ClassRel), "longleftrightarrow": symbol('⟷', ClassRel),
	"Longleftarrow": symbol('⟸', ClassRel), "Longrightarrow": symbol('⟹', ClassRel), "Longleftrightarrow": symbol('⟺', ClassRel),
	"longmapsto": symbol('⟼', ClassRel),
	"implies":    symbol('⟹', ClassRel), "impliedby": symbol('⟸', ClassRel), "iff": symbol('⟺', ClassRel),
	"uparrow": symbol('↑', ClassRel), "downarrow": symbol('↓', ClassRel), "updownarrow": symbol('↕', ClassRel),
	"Uparrow": symbol('⇑', ClassRel), "Downarrow": symbol('⇓', ClassRel), "Updownarrow": symbol('⇕', ClassRel),
	"hookleftarrow": symbol('↩', ClassRel), "hookrightarrow": symbol('↪', ClassRel),
}

var setCommands = map[string]command{
	"cup": symbol('∪', ClassBin), "cap": symbol('∩', ClassBin), "setminus": symbol('∖', ClassBin),
	"emptyset": symbol('∅', ClassOrd),
	"land":     symbol('∧', ClassBin), "wedge": symbol('∧', ClassBin), "lor": symbol('∨', ClassBin), "vee": symbol('∨', ClassBin),
	"lnot": symbol('¬', ClassOrd), "neg": symbol('¬', ClassOrd),
	"forall": symbol('∀', ClassOrd), "exists": symbol('∃', ClassOrd), "nexists": symbol('∄', ClassOrd),
	"complement": symbol('∁', ClassOrd),
}

var binaryCommands = map[string]command{
	"pm": symbol('±', ClassBin), "mp": symbol('∓', ClassBin), "times": symbol('×', ClassBin), "div": symbol('÷', ClassBin),
	"cdot": symbol('⋅', ClassBin), "ast": symbol('∗', ClassBin), "star": symbol('⋆', ClassBin), "circ": symbol('∘', ClassBin),
	"bullet": symbol('∙', ClassBin), "oplus": symbol('⊕', ClassBin), "ominus": symbol('⊖', ClassBin),
	"otimes": symbol('⊗', ClassBin), "odot": symbol('⊙', ClassBin),
}

var ordinaryCommands = map[string]command{
	"infty": symbol('∞', ClassOrd), "partial": symbol('∂', ClassOrd), "nabla": symbol('∇', ClassOrd),
	"ell": symbol('ℓ', ClassOrd), "hbar": symbol('ℏ', ClassOrd), "Re": symbol('ℜ', ClassOrd), "Im": symbol('ℑ', ClassOrd),
	"aleph": symbol('ℵ', ClassOrd), "angle": symbol('∠', ClassOrd), "triangle": symbol('△', ClassOrd),
	"prime": symbol('′', ClassOrd), "top": symbol('⊤', ClassOrd), "bot": symbol('⊥', ClassOrd),
	"dots": symbol('…', ClassInner), "ldots": symbol('…', ClassInner), "cdots": symbol('⋯', ClassInner),
	"vdots": symbol('⋮', ClassOrd), "ddots": symbol('⋱', ClassInner), "colon": symbol(':', ClassPunct),
	"{": symbol('{', ClassOpen), "}": symbol('}', ClassClose), "lbrace": symbol('{', ClassOpen), "rbrace": symbol('}', ClassClose),
	"#": symbol('#', ClassOrd), "$": symbol('$', ClassOrd), "%": symbol('%', ClassOrd), "&": symbol('&', ClassOrd),
	"_": symbol('_', ClassOrd), "|": symbol('‖', ClassOrd),
	"langle": symbol('⟨', ClassOpen), "rangle": symbol('⟩', ClassClose),
	"lfloor": symbol('⌊', ClassOpen), "rfloor": symbol('⌋', ClassClose),
	"lceil": symbol('⌈', ClassOpen), "rceil": symbol('⌉', ClassClose),
	"vert": symbol('|', ClassOrd), "Vert": symbol('‖', ClassOrd),
	"lvert": symbol('|', ClassOpen), "rvert": symbol('|', ClassClose),
	"lVert": symbol('‖', ClassOpen), "rVert": symbol('‖', ClassClose),
	"backslash": symbol('\\', ClassOrd),
}

var operatorCommands = map[string]command{
	"sum": largeOperator('∑', true), "prod": largeOperator('∏', true), "coprod": largeOperator('∐', true),
	"bigcup": largeOperator('⋃', true), "bigcap": largeOperator('⋂', true),
	"bigvee": largeOperator('⋁', true), "bigwedge": largeOperator('⋀', true),
	"bigoplus": largeOperator('⨁', true), "bigotimes": largeOperator('⨂', true), "bigodot": largeOperator('⨀', true),
	"int": largeOperator('∫', false), "iint": largeOperator('∬', false), "iiint": largeOperator('∭', false),
	"oint": largeOperator('∮', false),
	"sin":  function("sin", false), "cos": function("cos", false), "tan": function("tan", false),
	"cot": function("cot", false), "sec": function("sec", false), "csc": function("csc", false),
	"sinh": function("sinh", false), "cosh": function("cosh", false), "tanh": function("tanh", false),
	"coth": function("coth", false), "sech": function("sech", false), "csch": function("csch", false),
	"arcsin": function("arcsin", false), "arccos": function("arccos", false), "arctan": function("arctan", false),
	"log": function("log", false), "ln": function("ln", false), "lg": function("lg", false), "exp": function("exp", false),
	"arg": function("arg", false), "deg": function("deg", false), "dim": function("dim", false),
	"ker": function("ker", false), "hom": function("hom", false),
	"det": function("det", true), "gcd": function("gcd", true), "Pr": function("Pr", true),
	"lim": function("lim", true), "limsup": function("lim sup", true), "liminf": function("lim inf", true),
	"sup": function("sup", true), "inf": function("inf", true), "max": function("max", true), "min": function("min", true),
}

var structureCommands = map[string]command{
	"frac": {kind: kindFraction}, "dfrac": {kind: kindFraction}, "tfrac": {kind: kindFraction}, "binom": {kind: kindFraction},
	"sqrt": {kind: kindRadical},
	"left": {kind: kindLeft}, "middle": {kind: kindMiddle}, "right": {kind: kindRight},
	"big": sized(1.2, ClassOrd), "Big": sized(1.8, ClassOrd), "bigg": sized(2.4, ClassOrd), "Bigg": sized(3, ClassOrd),
	"bigl": sized(1.2, ClassOpen), "Bigl": sized(1.8, ClassOpen), "biggl": sized(2.4, ClassOpen), "Biggl": sized(3, ClassOpen),
	"bigr": sized(1.2, ClassClose), "Bigr": sized(1.8, ClassClose), "biggr": sized(2.4, ClassClose), "Biggr": sized(3, ClassClose),
	"bigm": sized(1.2, ClassRel), "Bigm": sized(1.8, ClassRel), "biggm": sized(2.4, ClassRel), "Biggm": sized(3, ClassRel),
	"hat": accent('\u0302', accentGlyph), "check": accent('\u030C', accentGlyph), "tilde": accent('\u0303', accentGlyph),
	"acute": accent('\u0301', accentGlyph), "grave": accent('\u0300', accentGlyph), "dot": accent('\u0307', accentGlyph),
	"ddot": accent('\u0308', accentGlyph), "breve": accent('\u0306', accentGlyph), "bar": accent('\u0304', accentGlyph),
	"vec":     accent('\u20D7', accentGlyph),
	"widehat": accent('\u0302', accentWide), "widetilde": accent('\u0303', accentWide),
	"overrightarrow": accent('\u20D7', accentWide), "overleftarrow": accent('\u20D6', accentWide),
	"overline": accent(0, accentOverline), "underline": accent(0, accentUnderline),
	"overbrace": accent('⏞', accentOverBrace), "underbrace": accent('⏟', accentUnderBrace),
	"mathrm": {kind: kindAlphabet}, "mathbf": {kind: kindAlphabet}, "mathit": {kind: kindAlphabet},
	"mathsf": {kind: kindAlphabet}, "mathtt": {kind: kindAlphabet}, "mathbb": {kind: kindAlphabet},
	"mathcal":      {kind: kindAlphabet},
	"text":         {kind: kindText},
	"operatorname": {kind: kindOperatorName},
	",":            space(3), "thinspace": space(3), ":": space(4), ">": space(4), "medspace": space(4),
	";": space(5), "thickspace": space(5), "!": space(-3), "negthinspace": space(-3),
	"quad": space(18), "qquad": space(36), "enspace": space(9), " ": space(6),
	"hspace":       {kind: kindExplicitSpace},
	"displaystyle": {kind: kindStyle}, "textstyle": {kind: kindStyle},
	"scriptstyle": {kind: kindStyle}, "scriptscriptstyle": {kind: kindStyle},
	"begin": {kind: kindBegin}, "end": {kind: kindEnd},
	"tag":    {kind: kindTag},
	"limits": {kind: kindLimits}, "nolimits": {kind: kindLimits},
}

var forbiddenCommands = map[string]command{
	"def": forbidden(reasonDefinition), "gdef": forbidden(reasonDefinition), "edef": forbidden(reasonDefinition),
	"xdef": forbidden(reasonDefinition), "let": forbidden(reasonDefinition), "futurelet": forbidden(reasonDefinition),
	"newcommand": forbidden(reasonDefinition), "renewcommand": forbidden(reasonDefinition),
	"providecommand": forbidden(reasonDefinition), "DeclareMathOperator": forbidden(reasonDefinition),
	"newenvironment": forbidden(reasonDefinition), "renewenvironment": forbidden(reasonDefinition),
	"input": forbidden(reasonFile), "include": forbidden(reasonFile), "InputIfFileExists": forbidden(reasonFile),
	"includegraphics": forbidden(reasonFile), "usepackage": forbidden(reasonFile), "RequirePackage": forbidden(reasonFile),
	"documentclass": forbidden(reasonFile), "openin": forbidden(reasonFile), "openout": forbidden(reasonFile),
	"read": forbidden(reasonFile), "write": forbidden(reasonFile), "immediate": forbidden(reasonFile),
	"special": forbidden(reasonProgram), "catcode": forbidden(reasonProgram), "csname": forbidden(reasonProgram),
	"endcsname": forbidden(reasonProgram), "expandafter": forbidden(reasonProgram), "global": forbidden(reasonProgram),
	"globaldefs": forbidden(reasonProgram), "loop": forbidden(reasonProgram), "repeat": forbidden(reasonProgram),
}

var commandTable = mergeCommands(greekCommands, relationCommands, arrowCommands, setCommands, binaryCommands,
	ordinaryCommands, operatorCommands, structureCommands, forbiddenCommands)

func mergeCommands(groups ...map[string]command) map[string]command {
	merged := make(map[string]command)
	for _, group := range groups {
		for name, entry := range group {
			merged[name] = entry
		}
	}
	return merged
}

type literal struct {
	value rune
	class Class
	large bool
}

var asciiLiterals = map[rune]literal{
	'+': {'+', ClassBin, false}, '-': {'−', ClassBin, false}, '*': {'∗', ClassBin, false}, '/': {'/', ClassOrd, false},
	'=': {'=', ClassRel, false}, '<': {'<', ClassRel, false}, '>': {'>', ClassRel, false},
	'(': {'(', ClassOpen, false}, ')': {')', ClassClose, false}, '[': {'[', ClassOpen, false}, ']': {']', ClassClose, false},
	'|': {'|', ClassOrd, false}, ',': {',', ClassPunct, false}, ';': {';', ClassPunct, false}, ':': {':', ClassRel, false},
	'!': {'!', ClassClose, false}, '?': {'?', ClassClose, false}, '.': {'.', ClassOrd, false}, '@': {'@', ClassOrd, false},
	'"': {'"', ClassOrd, false}, '`': {'`', ClassOrd, false},
}

var unicodeLiterals = buildUnicodeLiterals()

func buildUnicodeLiterals() map[rune]literal {
	literals := map[rune]literal{'−': {'−', ClassBin, false}, '∗': {'∗', ClassBin, false}}
	for _, entry := range commandTable {
		if entry.kind == kindLargeOperator {
			literals[entry.value] = literal{entry.value, ClassOp, true}
		}
		if entry.kind != kindSymbol || entry.value < 0x80 {
			continue
		}
		if existing, ok := literals[entry.value]; ok && existing.class < entry.class {
			continue
		}
		literals[entry.value] = literal{entry.value, entry.class, false}
	}
	return literals
}

var delimiterRunes = map[rune]bool{
	'(': true, ')': true, '[': true, ']': true, '{': true, '}': true, '⟨': true, '⟩': true, '|': true, '‖': true,
	'⌊': true, '⌋': true, '⌈': true, '⌉': true, '/': true, '\\': true,
	'↑': true, '↓': true, '↕': true, '⇑': true, '⇓': true, '⇕': true,
}

var largeOperatorLimits = buildLargeOperatorLimits()

func buildLargeOperatorLimits() map[rune]bool {
	limits := make(map[rune]bool)
	for _, entry := range operatorCommands {
		if entry.kind == kindLargeOperator {
			limits[entry.value] = entry.limits
		}
	}
	return limits
}

type environmentKind struct {
	topLevel    bool
	columns     int
	tags        bool
	arraySpec   bool
	pairsArg    bool
	singleRow   bool
	open, close rune
}

var environments = map[string]environmentKind{
	"matrix":      {},
	"pmatrix":     {open: '(', close: ')'},
	"bmatrix":     {open: '[', close: ']'},
	"Bmatrix":     {open: '{', close: '}'},
	"vmatrix":     {open: '|', close: '|'},
	"Vmatrix":     {open: '‖', close: '‖'},
	"smallmatrix": {},
	"array":       {arraySpec: true},
	"aligned":     {},
	"alignedat":   {pairsArg: true},
	"gathered":    {columns: 1},
	"cases":       {columns: 2, open: '{'},
	"align":       {topLevel: true, tags: true},
	"align*":      {topLevel: true, tags: true},
	"gather":      {topLevel: true, tags: true, columns: 1},
	"gather*":     {topLevel: true, tags: true, columns: 1},
	"equation":    {topLevel: true, tags: true, columns: 1, singleRow: true},
	"equation*":   {topLevel: true, tags: true, columns: 1, singleRow: true},
}
