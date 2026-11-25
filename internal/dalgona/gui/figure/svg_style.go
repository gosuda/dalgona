package figure

import (
	"strconv"
	"strings"
)

type paintKind uint8

const (
	paintNone paintKind = iota
	paintColor
	paintCurrent
	paintURL
)

type paintSpec struct {
	kind     paintKind
	color    rgba
	ref      string
	fallback *paintSpec
}

type fillRule uint8

const (
	ruleNonZero fillRule = iota
	ruleEvenOdd
)

type style struct {
	fill          paintSpec
	fillOpacity   float64
	fillRule      fillRule
	stroke        paintSpec
	strokeOpacity float64
	strokeWidth   length
	lineCap       lineCap
	lineJoin      lineJoin
	miterLimit    float64
	dashes        []length
	dashOffset    length
	color         rgba
	visible       bool
	fontSize      float64
	fontWeight    uint16
	italic        bool
	face          Face
	anchor        Anchor
	baseline      Baseline
	clipRule      fillRule
	markers       [3]string
	preserve      bool
	custom        map[string]string

	opacity       float64
	display       bool
	clipPath      string
	mask          string
	filter        string
	stopColor     paintSpec
	stopOpacity   float64
	floodColor    paintSpec
	floodOpacity  float64
	overflow      bool
	background    *rgba
	alignBaseline Baseline
	alignSet      bool
	multiply      bool
}

type lineCap uint8

const (
	capButt lineCap = iota
	capRound
	capSquare
)

type lineJoin uint8

const (
	joinMiter lineJoin = iota
	joinRound
	joinBevel
)

func initialStyle() *style {
	return &style{
		fill:          paintSpec{kind: paintColor, color: rgba{a: 1}},
		fillOpacity:   1,
		strokeOpacity: 1,
		strokeWidth:   length{value: 1},
		miterLimit:    4,
		color:         rgba{a: 1},
		visible:       true,
		fontSize:      16,
		fontWeight:    400,
		opacity:       1,
		display:       true,
		stopColor:     paintSpec{kind: paintColor, color: rgba{a: 1}},
		stopOpacity:   1,
		floodColor:    paintSpec{kind: paintColor, color: rgba{a: 1}},
		floodOpacity:  1,
	}
}

func (s *style) inherit() *style {
	child := *s
	initial := initialStyle()
	child.opacity, child.display, child.clipPath, child.mask, child.filter = 1, true, "", "", ""
	child.stopColor, child.stopOpacity = initial.stopColor, initial.stopOpacity
	child.floodColor, child.floodOpacity = initial.floodColor, initial.floodOpacity
	child.overflow, child.background, child.alignSet, child.multiply = false, nil, false, false
	return &child
}

type styleContext struct {
	profile Profile
	xHeight float64
}

type propertyParser func(ctx *styleContext, s, parent *style, value string) bool

type property struct {
	parse   propertyParser
	ignored string
}

var properties map[string]property

func init() {
	properties = map[string]property{
		"fill":                        {parse: parseFillPaint},
		"fill-opacity":                {parse: opacityInto(func(s *style) *float64 { return &s.fillOpacity })},
		"fill-rule":                   {parse: ruleInto(func(s *style) *fillRule { return &s.fillRule })},
		"clip-rule":                   {parse: ruleInto(func(s *style) *fillRule { return &s.clipRule })},
		"stroke":                      {parse: parseStrokePaint},
		"stroke-opacity":              {parse: opacityInto(func(s *style) *float64 { return &s.strokeOpacity })},
		"stroke-width":                {parse: parseStrokeWidth},
		"stroke-linecap":              {parse: parseLineCap},
		"stroke-linejoin":             {parse: parseLineJoin},
		"stroke-miterlimit":           {parse: parseMiterLimit},
		"stroke-dasharray":            {parse: parseDashArray},
		"stroke-dashoffset":           {parse: parseDashOffset},
		"opacity":                     {parse: opacityInto(func(s *style) *float64 { return &s.opacity })},
		"color":                       {parse: parseColorProperty},
		"display":                     {parse: parseDisplay},
		"visibility":                  {parse: parseVisibility},
		"font-size":                   {parse: parseFontSize},
		"font-weight":                 {parse: parseFontWeight},
		"font-style":                  {parse: parseFontStyle},
		"font-family":                 {parse: parseFontFamily},
		"text-anchor":                 {parse: parseTextAnchor},
		"dominant-baseline":           {parse: baselineInto(false)},
		"alignment-baseline":          {parse: baselineInto(true)},
		"clip-path":                   {parse: referenceInto(func(s *style) *string { return &s.clipPath })},
		"mask":                        {parse: referenceInto(func(s *style) *string { return &s.mask })},
		"filter":                      {parse: parseFilter},
		"marker-start":                {parse: referenceInto(func(s *style) *string { return &s.markers[0] })},
		"marker-mid":                  {parse: referenceInto(func(s *style) *string { return &s.markers[1] })},
		"marker-end":                  {parse: referenceInto(func(s *style) *string { return &s.markers[2] })},
		"marker":                      {parse: parseMarkerShorthand},
		"stop-color":                  {parse: colorPaintInto(func(s *style) *paintSpec { return &s.stopColor })},
		"stop-opacity":                {parse: opacityInto(func(s *style) *float64 { return &s.stopOpacity })},
		"flood-color":                 {parse: colorPaintInto(func(s *style) *paintSpec { return &s.floodColor })},
		"flood-opacity":               {parse: opacityInto(func(s *style) *float64 { return &s.floodOpacity })},
		"overflow":                    {parse: parseOverflow},
		"white-space":                 {parse: parseWhiteSpace},
		"background":                  {parse: parseBackground},
		"mix-blend-mode":              {parse: parseBlendMode},
		"letter-spacing":              {parse: neutral("normal", "0", "0px")},
		"word-spacing":                {parse: neutral("normal", "0", "0px")},
		"text-decoration":             {parse: neutral("none")},
		"baseline-shift":              {parse: neutral("baseline", "0", "0px")},
		"font-variant":                {parse: neutral("normal")},
		"font-stretch":                {parse: neutral("normal", "100%")},
		"writing-mode":                {parse: neutral("horizontal-tb", "lr-tb", "lr")},
		"direction":                   {parse: neutral("ltr")},
		"unicode-bidi":                {parse: neutral("normal")},
		"paint-order":                 {parse: neutral("normal", "fill stroke markers", "fill")},
		"vector-effect":               {parse: neutral("none")},
		"transform":                   {parse: neutral("none")},
		"color-interpolation":         {parse: neutral("auto", "srgb", "sRGB")},
		"shape-rendering":             {ignored: "rendering hint"},
		"text-rendering":              {ignored: "rendering hint"},
		"color-rendering":             {ignored: "rendering hint"},
		"image-rendering":             {ignored: "rendering hint"},
		"pointer-events":              {ignored: "interaction hint"},
		"cursor":                      {ignored: "interaction hint"},
		"isolation":                   {parse: neutral("auto", "isolate")},
		"color-interpolation-filters": {ignored: "filter hint"},
	}
}

func neutral(values ...string) propertyParser {
	return func(_ *styleContext, _, _ *style, v string) bool {
		for _, ok := range values {
			if strings.EqualFold(v, ok) {
				return true
			}
		}
		return false
	}
}

func opacityInto(field func(*style) *float64) propertyParser {
	return func(_ *styleContext, s, _ *style, v string) bool {
		f, ok := fraction(strings.TrimSpace(v), 1)
		*field(s) = f
		return ok
	}
}

func ruleInto(field func(*style) *fillRule) propertyParser {
	return func(_ *styleContext, s, _ *style, v string) bool {
		switch v {
		case "nonzero":
			*field(s) = ruleNonZero
		case "evenodd":
			*field(s) = ruleEvenOdd
		default:
			return false
		}
		return true
	}
}

func referenceInto(field func(*style) *string) propertyParser {
	return func(_ *styleContext, s, _ *style, v string) bool {
		if v == "none" {
			*field(s) = ""
			return true
		}
		id, ok := localURL(v)
		*field(s) = id
		return ok
	}
}

func localURL(v string) (string, bool) {
	inner, ok := strings.CutPrefix(v, "url(")
	if !ok || !strings.HasSuffix(inner, ")") {
		return "", false
	}
	inner = strings.Trim(strings.TrimSpace(inner[:len(inner)-1]), `"'`)
	id, ok := strings.CutPrefix(inner, "#")
	return id, ok && id != ""
}

func parseMarkerShorthand(ctx *styleContext, s, parent *style, v string) bool {
	for i := range s.markers {
		if !referenceInto(func(s *style) *string { return &s.markers[i] })(ctx, s, parent, v) {
			return false
		}
	}
	return true
}

func parsePaint(v string, current rgba) (paintSpec, bool) {
	v = strings.TrimSpace(v)
	switch strings.ToLower(v) {
	case "none":
		return paintSpec{kind: paintNone}, true
	case "currentcolor":
		return paintSpec{kind: paintCurrent}, true
	}
	if strings.HasPrefix(v, "url(") {
		end := strings.IndexByte(v, ')')
		id, ok := localURL(v[:end+1])
		spec := paintSpec{kind: paintURL, ref: id}
		if rest := strings.TrimSpace(v[end+1:]); ok && rest != "" {
			fallback, fok := parsePaint(rest, current)
			spec.fallback, ok = &fallback, fok && fallback.kind != paintURL
		}
		return spec, ok
	}
	c, ok := parseColorValue(v, current)
	return paintSpec{kind: paintColor, color: c}, ok
}

func parseColorValue(v string, current rgba) (rgba, bool) {
	if strings.EqualFold(strings.TrimSpace(v), "currentcolor") {
		return current, true
	}
	if inner, ok := strings.CutPrefix(strings.TrimSpace(v), "color-mix("); ok && strings.HasSuffix(inner, ")") {
		return parseColorMix(inner[:len(inner)-1], current)
	}
	return parseColor(v)
}

func parseColorMix(body string, current rgba) (rgba, bool) {
	parts := splitTopLevel(body, ',')
	if len(parts) != 3 || strings.Join(strings.Fields(parts[0]), " ") != "in srgb" {
		return rgba{}, false
	}
	c1, p1, ok1 := mixComponent(parts[1], current)
	c2, p2, ok2 := mixComponent(parts[2], current)
	if !ok1 || !ok2 {
		return rgba{}, false
	}
	p1, p2 = mixWeights(p1, p2)
	total := p1 + p2
	if total <= 0 {
		return rgba{}, false
	}
	return mixPremultiplied(c1, c2, p1/total, p2/total).withAlpha(min(total, 1)), true
}

func mixWeights(p1, p2 float64) (float64, float64) {
	switch {
	case p1 < 0 && p2 < 0:
		return 0.5, 0.5
	case p1 < 0:
		return 1 - p2, p2
	case p2 < 0:
		return p1, 1 - p1
	}
	return p1, p2
}

func mixComponent(s string, current rgba) (rgba, float64, bool) {
	fields := strings.Fields(s)
	percent := -1.0
	if n := len(fields); n > 1 && strings.HasSuffix(fields[n-1], "%") {
		v, err := strconv.ParseFloat(strings.TrimSuffix(fields[n-1], "%"), 64)
		if err != nil || v < 0 || v > 100 {
			return rgba{}, 0, false
		}
		percent, fields = v/100, fields[:n-1]
	}
	c, ok := parseColorValue(strings.Join(fields, " "), current)
	return c, percent, ok
}

func mixPremultiplied(a, b rgba, wa, wb float64) rgba {
	alpha := a.a*wa + b.a*wb
	if alpha == 0 {
		return rgba{}
	}
	mix := func(x, y float64) float64 { return (x*a.a*wa + y*b.a*wb) / alpha }
	return rgba{r: mix(a.r, b.r), g: mix(a.g, b.g), b: mix(a.b, b.b), a: alpha}
}

func parseFillPaint(_ *styleContext, s, _ *style, v string) bool {
	p, ok := parsePaint(v, s.color)
	s.fill = p
	return ok
}

func parseStrokePaint(_ *styleContext, s, _ *style, v string) bool {
	p, ok := parsePaint(v, s.color)
	s.stroke = p
	return ok
}

func colorPaintInto(field func(*style) *paintSpec) propertyParser {
	return func(_ *styleContext, s, _ *style, v string) bool {
		p, ok := parsePaint(v, s.color)
		*field(s) = p
		return ok && p.kind != paintURL
	}
}

func parseColorProperty(_ *styleContext, s, parent *style, v string) bool {
	c, ok := parseColorValue(v, parent.color)
	s.color = c
	return ok
}

func parseStrokeWidth(_ *styleContext, s, _ *style, v string) bool {
	l, ok := parseLength(v)
	s.strokeWidth = l
	return ok && l.value >= 0
}

func parseLineCap(_ *styleContext, s, _ *style, v string) bool {
	c, ok := map[string]lineCap{"butt": capButt, "round": capRound, "square": capSquare}[v]
	s.lineCap = c
	return ok
}

func parseLineJoin(_ *styleContext, s, _ *style, v string) bool {
	j, ok := map[string]lineJoin{"miter": joinMiter, "round": joinRound, "bevel": joinBevel, "miter-clip": joinMiter}[v]
	s.lineJoin = j
	return ok
}

func parseMiterLimit(_ *styleContext, s, _ *style, v string) bool {
	f, err := strconv.ParseFloat(v, 64)
	s.miterLimit = f
	return err == nil && f >= 1 && withinLimit(f)
}

func parseDashArray(_ *styleContext, s, _ *style, v string) bool {
	s.dashes = nil
	if v == "none" {
		return true
	}
	for _, part := range strings.FieldsFunc(v, isListSeparator) {
		l, ok := parseLength(part)
		if !ok || l.value < 0 || len(s.dashes) >= 64 {
			return false
		}
		s.dashes = append(s.dashes, l)
	}
	return true
}

func parseDashOffset(_ *styleContext, s, _ *style, v string) bool {
	l, ok := parseLength(v)
	s.dashOffset = l
	return ok
}

func parseDisplay(_ *styleContext, s, _ *style, v string) bool {
	s.display = v != "none"
	return v != ""
}

func parseVisibility(_ *styleContext, s, _ *style, v string) bool {
	switch v {
	case "visible":
		s.visible = true
	case "hidden", "collapse":
		s.visible = false
	default:
		return false
	}
	return true
}

func parseFontSize(ctx *styleContext, s, parent *style, v string) bool {
	if size, ok := absoluteFontSizes[v]; ok {
		s.fontSize = size
		return true
	}
	l, ok := parseLength(v)
	if !ok || l.value <= 0 {
		return false
	}
	s.fontSize = l.resolve(lengthBasis{fontSize: parent.fontSize, xHeight: ctx.xHeight, percent: parent.fontSize})
	return withinLimit(s.fontSize)
}

var absoluteFontSizes = map[string]float64{
	"xx-small": 9, "x-small": 10, "small": 13, "medium": 16, "large": 18, "x-large": 24, "xx-large": 32,
}

func parseFontWeight(_ *styleContext, s, parent *style, v string) bool {
	switch v {
	case "normal":
		s.fontWeight = 400
	case "bold":
		s.fontWeight = 700
	case "bolder":
		s.fontWeight = min(parent.fontWeight+300, 900)
	case "lighter":
		s.fontWeight = max(parent.fontWeight, 400) - 300
	default:
		w, err := strconv.ParseFloat(v, 64)
		if err != nil || !(w >= 1 && w <= 1000) {
			return false
		}
		s.fontWeight = uint16(w)
	}
	return true
}

func parseFontStyle(_ *styleContext, s, _ *style, v string) bool {
	switch v {
	case "normal":
		s.italic = false
	case "italic", "oblique":
		s.italic = true
	default:
		return false
	}
	return true
}

var monoFamilies = []string{"mono", "code", "courier", "consolas", "menlo"}

func parseFontFamily(_ *styleContext, s, _ *style, v string) bool {
	s.face = FaceSans
	for _, family := range strings.Split(v, ",") {
		name := strings.ToLower(strings.Trim(strings.TrimSpace(family), `"'`))
		for _, mono := range monoFamilies {
			if strings.Contains(name, mono) {
				s.face = FaceMono
			}
		}
		applyEngineFamily(s, name)
	}
	return strings.TrimSpace(v) != ""
}

func applyEngineFamily(s *style, name string) {
	_, variant, ok := strings.Cut(name, "-font-")
	if !ok || !strings.HasPrefix(name, "d2-") {
		return
	}
	s.italic = strings.Contains(variant, "italic")
	switch {
	case strings.Contains(variant, "semibold"):
		s.fontWeight = 600
	case strings.Contains(variant, "bold"):
		s.fontWeight = 700
	default:
		s.fontWeight = 400
	}
}

func parseTextAnchor(_ *styleContext, s, _ *style, v string) bool {
	a, ok := map[string]Anchor{"start": AnchorStart, "middle": AnchorMiddle, "end": AnchorEnd}[v]
	s.anchor = a
	return ok
}

var baselines = map[string]Baseline{
	"auto": BaselineAlphabetic, "baseline": BaselineAlphabetic, "alphabetic": BaselineAlphabetic,
	"middle": BaselineMiddle, "central": BaselineCentral, "hanging": BaselineHanging,
	"text-before-edge": BaselineTextTop, "text-top": BaselineTextTop, "before-edge": BaselineTextTop,
	"text-after-edge": BaselineTextBottom, "text-bottom": BaselineTextBottom, "after-edge": BaselineTextBottom,
	"ideographic": BaselineIdeographic, "mathematical": BaselineMathematical,
}

func baselineInto(alignment bool) propertyParser {
	return func(_ *styleContext, s, _ *style, v string) bool {
		b, ok := baselines[v]
		if alignment {
			s.alignBaseline, s.alignSet = b, v != "auto" && v != "baseline"
		} else {
			s.baseline = b
		}
		return ok
	}
}

func parseFilter(ctx *styleContext, s, _ *style, v string) bool {
	if v == "none" {
		s.filter = ""
		return true
	}
	if ctx.profile != ProfileD2 {
		return false
	}
	id, ok := localURL(v)
	s.filter = id
	return ok
}

func parseOverflow(_ *styleContext, s, _ *style, v string) bool {
	switch v {
	case "visible", "auto":
		s.overflow = true
	case "hidden", "scroll", "clip":
		s.overflow = false
	default:
		return false
	}
	return true
}

func parseWhiteSpace(_ *styleContext, s, _ *style, v string) bool {
	switch v {
	case "normal", "nowrap":
		s.preserve = false
	case "pre", "pre-wrap", "break-spaces":
		s.preserve = true
	default:
		return false
	}
	return true
}

func parseBackground(_ *styleContext, s, _ *style, v string) bool {
	c, ok := parseColorValue(v, s.color)
	s.background = &c
	return ok
}

func parseBlendMode(ctx *styleContext, s, _ *style, v string) bool {
	s.multiply = v == "multiply"
	return v == "normal" || (ctx.profile == ProfileD2 && s.multiply)
}
