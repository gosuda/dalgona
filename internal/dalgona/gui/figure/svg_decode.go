package figure

import (
	"math"
	"strings"
)

type DecodeOptions struct {
	Profile  Profile
	Scale    float64
	XHeight  float64
	overlays map[string]overlay
}

type decoder struct {
	opts        DecodeOptions
	ctx         styleContext
	list        *List
	ids         map[string]*svgNode
	sheet       []cssRule
	expanded    int
	references  []*svgNode
	override    *rgba
	rasterBytes int
	domStyles   map[*svgNode]*style
	gradients   map[string]int32
}

type renderState struct {
	style    *style
	ctm      matrix
	viewport vec
	depth    int
}

const deviceTolerance = 0.2

func DecodeSVG(src []byte, opts DecodeOptions) (*List, error) {
	limit := SourceLimit
	if opts.Profile.engine() {
		limit = EngineOutputLimit
	}
	if len(src) > limit {
		return nil, fail(FailureLimit, "SVG has %d bytes; the limit is %d", len(src), limit)
	}
	opts = opts.withDefaults()
	root, err := parseTree(src)
	if err != nil {
		return nil, err
	}
	d := &decoder{
		opts:      opts,
		ctx:       styleContext{profile: opts.Profile, xHeight: opts.XHeight},
		list:      &List{},
		ids:       map[string]*svgNode{},
		domStyles: map[*svgNode]*style{},
		gradients: map[string]int32{},
	}
	if err := d.prepare(root); err != nil {
		return nil, err
	}
	if err := d.document(root); err != nil {
		return nil, err
	}
	if n := d.list.Operations(); n > OperationLimit {
		return nil, fail(FailureLimit, "figure needs %d display operations; the limit is %d", n, OperationLimit)
	}
	return d.list, nil
}

func (o DecodeOptions) withDefaults() DecodeOptions {
	if !(o.Scale > 0 && o.Scale <= 16) {
		o.Scale = 1
	}
	if !(o.XHeight > 0 && o.XHeight < 2) {
		o.XHeight = 0.5
	}
	return o
}

func (d *decoder) prepare(root *svgNode) error {
	if root.name != "svg" {
		return failAt(FailureInvalid, root.span, "root element is <%s>; an SVG figure starts with <svg>", root.name)
	}
	order := 0
	return d.visitTree(root, &order)
}

func (d *decoder) visitTree(n *svgNode, order *int) error {
	if n.isText {
		return nil
	}
	if err := d.checkElement(n); err != nil {
		return err
	}
	if id, ok := n.attr("id"); ok {
		if _, dup := d.ids[id]; !dup {
			d.ids[id] = n
		}
	}
	if n.name == "style" {
		rules, problem := parseSheet(n.textContent(), order)
		if problem != "" {
			return failAt(FailureInvalid, n.span, "%s", problem)
		}
		d.sheet = append(d.sheet, rules...)
		return nil
	}
	for _, c := range n.children {
		if err := d.visitTree(c, order); err != nil {
			return err
		}
	}
	return nil
}

var forbiddenElements = map[string]string{
	"script": "scripts", "foreignObject": "foreign content", "image": "resource-loading elements",
	"feImage": "resource-loading elements", "iframe": "resource-loading elements", "object": "resource-loading elements",
	"embed": "resource-loading elements", "audio": "media elements", "video": "media elements", "link": "resource-loading elements",
	"base": "resource-loading elements", "meta": "metadata directives", "form": "form controls", "input": "form controls",
	"animate": "animation", "animateMotion": "animation", "animateTransform": "animation", "animateColor": "animation",
	"set": "animation", "mpath": "animation", "discard": "animation", "font-face-uri": "resource-loading elements",
}

var profileElements = map[string]bool{
	"svg": true, "g": true, "defs": true, "symbol": true, "use": true, "title": true, "desc": true,
	"path": true, "rect": true, "circle": true, "ellipse": true, "line": true, "polyline": true, "polygon": true,
	"text": true, "tspan": true, "linearGradient": true, "radialGradient": true, "stop": true,
	"clipPath": true, "mask": true, "marker": true,
}

var engineElements = map[Profile]map[string]bool{
	ProfileMermaid: {"style": true},
	ProfileD2: {
		"style": true, "a": true, "filter": true, "feGaussianBlur": true, "feFlood": true,
		"feComposite": true, "feOffset": true, "feBlend": true, "feMerge": true, "feMergeNode": true,
	},
}

func (d *decoder) checkElement(n *svgNode) error {
	if reason, ok := forbiddenElements[n.name]; ok {
		return failAt(FailureForbidden, n.span, "<%s> is refused because SVG figures exclude %s; remove it", n.name, reason)
	}
	if !profileElements[n.name] && !engineElements[d.opts.Profile][n.name] {
		return failAt(FailureInvalid, n.span, "<%s> is outside the static SVG profile; remove it", n.name)
	}
	for _, a := range n.attrs {
		if err := d.checkAttribute(n, a); err != nil {
			return err
		}
	}
	return nil
}

var conditionalAttributes = map[string]bool{"requiredFeatures": true, "requiredExtensions": true, "systemLanguage": true}

func (d *decoder) checkAttribute(n *svgNode, a svgAttr) error {
	switch {
	case strings.HasPrefix(strings.ToLower(a.name), "on"):
		return failAt(FailureForbidden, n.span, "event attribute %s on <%s> is refused; remove it", a.name, n.name)
	case a.name == "href" && n.name == "a":
		return checkLink(n, a.value)
	case a.name == "href" && !strings.HasPrefix(strings.TrimSpace(a.value), "#"):
		return failAt(FailureForbidden, n.span, "<%s> refers to %s outside the figure; only local #id references are allowed", n.name, quoted(a.value))
	case conditionalAttributes[a.name]:
		return failAt(FailureInvalid, n.span, "conditional attribute %s on <%s> is outside the static SVG profile", a.name, n.name)
	case a.space == "" && externalURL(a.value):
		return failAt(FailureForbidden, n.span, "attribute %s on <%s> refers outside the figure; only url(#id) is allowed", a.name, n.name)
	}
	return nil
}

func checkLink(n *svgNode, target string) error {
	target = strings.TrimSpace(target)
	for _, scheme := range [...]string{"https://", "http://", "mailto:", "#"} {
		if strings.HasPrefix(strings.ToLower(target), scheme) {
			return nil
		}
	}
	return failAt(FailureForbidden, n.span, "link target %s is not an http, https, mailto, or local link", quoted(target))
}

func externalURL(value string) bool {
	for {
		i := strings.Index(value, "url(")
		if i < 0 {
			return false
		}
		value = value[i+4:]
		if !strings.HasPrefix(strings.TrimLeft(strings.TrimSpace(value), `"'`), "#") {
			return true
		}
	}
}

func (d *decoder) document(root *svgNode) error {
	st, err := d.computeStyle(root, initialStyle())
	if err != nil {
		return err
	}
	vb, hasViewBox, err := viewBoxOf(root)
	if err != nil {
		return err
	}
	size, err := d.figureSize(root, st, vb, hasViewBox)
	if err != nil {
		return err
	}
	d.list.Width, d.list.Height = float32(size.x), float32(size.y)
	d.list.Title = titleOf(root)
	if st.background != nil && st.background.a > 0 {
		if err := d.fillRect(vec{0, 0}, size, *st.background); err != nil {
			return err
		}
	}
	state := renderState{style: st, ctm: identity, viewport: size}
	return d.viewportContent(root, state, size, vb, hasViewBox)
}

func (d *decoder) figureSize(root *svgNode, st *style, vb viewBox, hasViewBox bool) (vec, error) {
	w, h := 300.0, 150.0
	if hasViewBox {
		w, h = vb.width, vb.height
	}
	basis := lengthBasis{fontSize: st.fontSize, xHeight: d.opts.XHeight}
	w = d.optionalLength(root, "width", w, basis)
	h = d.optionalLength(root, "height", h, basis)
	if !(w > 0 && w <= CoordinateLimit && h > 0 && h <= CoordinateLimit) {
		return vec{}, failAt(FailureLimit, root.span, "figure size %gx%g is outside 1 and %d pixels", w, h, CoordinateLimit)
	}
	return vec{w, h}, nil
}

func titleOf(n *svgNode) string {
	for _, c := range n.children {
		if c.name == "title" {
			return strings.TrimSpace(c.textContent())
		}
	}
	return ""
}

func (d *decoder) fillRect(lo, hi vec, c rgba) error {
	d.list.Paths = append(d.list.Paths, rectShape(lo, hi.sub(lo)).path())
	index, err := listIndex(len(d.list.Paths) - 1)
	if err != nil {
		return err
	}
	d.list.Ops = append(d.list.Ops, Op{Kind: OpFill, Index: index, Paint: SolidPaint(c.color())})
	return nil
}

func (d *decoder) optionalLength(n *svgNode, name string, fallback float64, basis lengthBasis) float64 {
	v, ok := n.attr(name)
	if !ok || v == "auto" {
		return fallback
	}
	l, ok := parseLength(v)
	if !ok {
		return math.NaN()
	}
	if l.unit == unitPercent {
		return fallback * l.value / 100
	}
	return l.resolve(basis)
}

type viewBox struct{ x, y, width, height float64 }

func viewBoxOf(n *svgNode) (viewBox, bool, error) {
	raw, ok := n.attr("viewBox")
	if !ok {
		return viewBox{}, false, nil
	}
	v, ok := parseNumbers(raw, 4)
	if !ok || len(v) != 4 || v[2] <= 0 || v[3] <= 0 {
		return viewBox{}, false, failAt(FailureInvalid, n.span, "viewBox %s needs four numbers with a positive width and height", quoted(raw))
	}
	for _, x := range v {
		if !withinLimit(x) {
			return viewBox{}, false, failAt(FailureLimit, n.span, "viewBox %s exceeds the coordinate limit", quoted(raw))
		}
	}
	return viewBox{v[0], v[1], v[2], v[3]}, true, nil
}

type aspectRatio struct {
	align string
	slice bool
}

func aspectRatioOf(n *svgNode) (aspectRatio, error) {
	raw, _ := n.attr("preserveAspectRatio")
	fields := strings.Fields(raw)
	ratio := aspectRatio{align: "xMidYMid"}
	if len(fields) > 0 && fields[0] == "defer" {
		fields = fields[1:]
	}
	if len(fields) > 0 {
		ratio.align = fields[0]
	}
	if len(fields) > 1 {
		ratio.slice = fields[1] == "slice"
		if fields[1] != "slice" && fields[1] != "meet" {
			return aspectRatio{}, failAt(FailureInvalid, n.span, "preserveAspectRatio %s is malformed", quoted(raw))
		}
	}
	return ratio, nil
}

func aspectTransform(n *svgNode, vb viewBox, size vec) (matrix, error) {
	ratio, err := aspectRatioOf(n)
	if err != nil {
		return matrix{}, err
	}
	sx, sy := size.x/vb.width, size.y/vb.height
	if ratio.align == "none" {
		return scale(sx, sy).mul(translate(-vb.x, -vb.y)), nil
	}
	s := math.Min(sx, sy)
	if ratio.slice {
		s = math.Max(sx, sy)
	}
	fx, fy, ok := alignFactors(ratio.align)
	if !ok {
		return matrix{}, failAt(FailureInvalid, n.span, "preserveAspectRatio alignment %s is not registered", quoted(ratio.align))
	}
	tx, ty := (size.x-vb.width*s)*fx, (size.y-vb.height*s)*fy
	return translate(tx, ty).mul(scale(s, s)).mul(translate(-vb.x, -vb.y)), nil
}

func alignFactors(align string) (float64, float64, bool) {
	if len(align) != 8 || align[0] != 'x' || align[4] != 'Y' {
		return 0, 0, false
	}
	factor := map[string]float64{"Min": 0, "Mid": 0.5, "Max": 1}
	fx, okX := factor[align[1:4]]
	fy, okY := factor[align[5:8]]
	return fx, fy, okX && okY
}

func (d *decoder) viewportContent(n *svgNode, st renderState, size vec, vb viewBox, hasViewBox bool) error {
	pushes := 0
	if !st.style.overflow {
		if err := d.pushClip(rectShape(vec{0, 0}, size)); err != nil {
			return err
		}
		pushes++
	}
	if hasViewBox {
		m, err := aspectTransform(n, vb, size)
		if err != nil {
			return err
		}
		if !d.pushTransform(m, &st) {
			return failAt(FailureLimit, n.span, "viewBox transform is outside the coordinate limit")
		}
		pushes++
		st.viewport = vec{vb.width, vb.height}
	} else {
		st.viewport = size
	}
	err := d.children(n, st)
	d.pop(pushes)
	return err
}

func (d *decoder) children(n *svgNode, st renderState) error {
	for _, c := range n.children {
		if err := d.element(c, st); err != nil {
			return err
		}
	}
	return nil
}

func (d *decoder) pushTransform(m matrix, st *renderState) bool {
	if !m.finite() {
		return false
	}
	st.ctm = st.ctm.mul(m)
	if !st.ctm.finite() {
		return false
	}
	d.list.Ops = append(d.list.Ops, Op{Kind: OpTransform, Transform: m.affine()})
	return true
}

func (d *decoder) pushClip(s *shape) error {
	d.list.Paths = append(d.list.Paths, s.path())
	index, err := listIndex(len(d.list.Paths) - 1)
	if err != nil {
		return err
	}
	d.list.Ops = append(d.list.Ops, Op{Kind: OpClip, Index: index})
	return nil
}

func (d *decoder) pop(n int) {
	for range n {
		d.list.Ops = append(d.list.Ops, Op{Kind: OpPop})
	}
}

func listIndex(i int) (int32, error) {
	if i < 0 || i > math.MaxInt32 {
		return 0, fail(FailureLimit, "display list index %d is outside 0 and %d; simplify the figure", i, math.MaxInt32)
	}
	return int32(i), nil
}

var containerElements = map[string]bool{"g": true, "a": true}

var definitionElements = map[string]bool{
	"defs": true, "symbol": true, "clipPath": true, "mask": true, "marker": true, "linearGradient": true,
	"radialGradient": true, "stop": true, "title": true, "desc": true, "style": true, "filter": true,
	"feGaussianBlur": true, "feFlood": true, "feComposite": true, "feOffset": true, "feBlend": true,
	"feMerge": true, "feMergeNode": true, "tspan": true,
}

func (d *decoder) element(n *svgNode, parent renderState) error {
	if n.isText || definitionElements[n.name] {
		return nil
	}
	if parent.depth >= SVGDepthLimit {
		return failAt(FailureLimit, n.span, "figure nests deeper than %d levels", SVGDepthLimit)
	}
	d.expanded++
	if d.expanded > SVGElementLimit {
		return failAt(FailureLimit, n.span, "figure expands to more than %d elements; reduce <use> references", SVGElementLimit)
	}
	st, err := d.computeStyle(n, parent.style)
	if err != nil {
		return err
	}
	if !st.display {
		return nil
	}
	state := renderState{style: st, ctm: parent.ctm, viewport: parent.viewport, depth: parent.depth + 1}
	return d.group(n, state, func(inner renderState) error { return d.content(n, inner) })
}

func (d *decoder) content(n *svgNode, st renderState) error {
	switch {
	case containerElements[n.name]:
		if err := d.children(n, st); err != nil {
			return err
		}
		return d.overlay(n, st)
	case n.name == "svg":
		return d.nestedViewport(n, st)
	case n.name == "use":
		return d.use(n, st)
	case n.name == "text":
		return d.text(n, st)
	}
	return d.shapeElement(n, st)
}

func (d *decoder) localTransform(n *svgNode) (matrix, error) {
	raw, ok := n.attr("transform")
	if !ok || n.name == "svg" {
		return identity, nil
	}
	m, ok := parseTransform(raw)
	if !ok {
		return matrix{}, failAt(FailureInvalid, n.span, "transform %s is malformed or not finite", quoted(raw))
	}
	return m, nil
}

func (d *decoder) group(n *svgNode, st renderState, body func(renderState) error) error {
	start := len(d.list.Ops)
	pushes, cut, err := d.enter(n, &st)
	if err == nil && !cut.hidden() {
		err = d.drawCut(n, st, cut, body)
	}
	d.pop(pushes)
	if err == nil && st.style.multiply {
		err = d.multiply(n, start)
	}
	return err
}

func (d *decoder) enter(n *svgNode, st *renderState) (int, cutout, error) {
	m, err := d.localTransform(n)
	if err != nil {
		return 0, cutout{}, err
	}
	pushes := 0
	if !m.isIdentity() {
		if !d.pushTransform(m, st) {
			return 0, cutout{}, failAt(FailureLimit, n.span, "transform on <%s> exceeds the coordinate limit", n.name)
		}
		pushes++
	}
	cut, err := d.cutout(n, *st)
	if err != nil {
		return pushes, cut, err
	}
	for _, c := range cut.clips {
		if err := d.pushClip(c); err != nil {
			return pushes, cut, err
		}
		pushes++
	}
	if st.style.opacity < 1 {
		d.list.Ops = append(d.list.Ops, Op{Kind: OpOpacity, Alpha: float32(st.style.opacity)})
		pushes++
	}
	return pushes, cut, nil
}

func (d *decoder) drawCut(n *svgNode, st renderState, cut cutout, body func(renderState) error) error {
	draw := func() error { return d.filtered(n, st, body) }
	if !cut.masked {
		return draw()
	}
	return d.masked(cut.mask, draw)
}

func (d *decoder) nestedViewport(n *svgNode, st renderState) error {
	basis := d.basis(st, 0)
	x := d.lengthAttr(n, "x", 0, basis, st.viewport.x)
	y := d.lengthAttr(n, "y", 0, basis, st.viewport.y)
	w := d.lengthAttr(n, "width", st.viewport.x, basis, st.viewport.x)
	h := d.lengthAttr(n, "height", st.viewport.y, basis, st.viewport.y)
	if !(w > 0 && h > 0) {
		return nil
	}
	vb, hasViewBox, err := viewBoxOf(n)
	if err != nil {
		return err
	}
	pushes := 0
	if x != 0 || y != 0 {
		if !d.pushTransform(translate(x, y), &st) {
			return failAt(FailureLimit, n.span, "nested viewport exceeds the coordinate limit")
		}
		pushes++
	}
	err = d.viewportContent(n, st, vec{w, h}, vb, hasViewBox)
	d.pop(pushes)
	return err
}

func (d *decoder) basis(st renderState, percent float64) lengthBasis {
	return lengthBasis{fontSize: st.style.fontSize, xHeight: d.opts.XHeight, percent: percent}
}

func (d *decoder) lengthAttr(n *svgNode, name string, fallback float64, basis lengthBasis, percent float64) float64 {
	v, ok := n.attr(name)
	if !ok {
		return fallback
	}
	l, ok := parseLength(v)
	if !ok {
		return math.NaN()
	}
	basis.percent = percent
	return l.resolve(basis)
}

func diagonal(v vec) float64 { return math.Hypot(v.x, v.y) / math.Sqrt2 }

func (d *decoder) reference(n *svgNode, id string) (*svgNode, error) {
	target, ok := d.ids[id]
	if !ok {
		return nil, failAt(FailureInvalid, n.span, "<%s> refers to #%s, which the figure does not define", n.name, id)
	}
	for _, active := range d.references {
		if active == target {
			return nil, failAt(FailureInvalid, n.span, "<%s> reference to #%s forms a cycle; break the cycle", n.name, id)
		}
	}
	return target, nil
}

func (d *decoder) use(n *svgNode, st renderState) error {
	raw, _ := n.href()
	target, err := d.reference(n, strings.TrimPrefix(strings.TrimSpace(raw), "#"))
	if err != nil {
		return err
	}
	basis := d.basis(st, 0)
	x := d.lengthAttr(n, "x", 0, basis, st.viewport.x)
	y := d.lengthAttr(n, "y", 0, basis, st.viewport.y)
	if !withinLimit(x) || !withinLimit(y) {
		return failAt(FailureInvalid, n.span, "<use> position is malformed or exceeds the coordinate limit")
	}
	pushes := 0
	if x != 0 || y != 0 {
		if !d.pushTransform(translate(x, y), &st) {
			return failAt(FailureLimit, n.span, "<use> position exceeds the coordinate limit")
		}
		pushes = 1
	}
	d.references = append(d.references, target)
	err = d.instantiate(n, target, st)
	d.references = d.references[:len(d.references)-1]
	d.pop(pushes)
	return err
}

func (d *decoder) instantiate(use, target *svgNode, st renderState) error {
	if target.name != "symbol" {
		return d.element(target, st)
	}
	d.expanded++
	if d.expanded > SVGElementLimit {
		return failAt(FailureLimit, use.span, "figure expands to more than %d elements; reduce <use> references", SVGElementLimit)
	}
	sym, err := d.computeStyle(target, st.style)
	if err != nil {
		return err
	}
	basis := d.basis(st, 0)
	w := d.lengthAttr(use, "width", d.lengthAttr(target, "width", st.viewport.x, basis, st.viewport.x), basis, st.viewport.x)
	h := d.lengthAttr(use, "height", d.lengthAttr(target, "height", st.viewport.y, basis, st.viewport.y), basis, st.viewport.y)
	if !(w > 0 && h > 0) {
		return nil
	}
	vb, hasViewBox, err := viewBoxOf(target)
	if err != nil {
		return err
	}
	inner := renderState{style: sym, ctm: st.ctm, viewport: st.viewport, depth: st.depth + 1}
	return d.viewportContent(target, inner, vec{w, h}, vb, hasViewBox)
}
