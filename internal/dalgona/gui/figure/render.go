package figure

import (
	"context"

	"modernc.org/quickjs"
)

type Kind uint8

const (
	KindMermaid Kind = iota + 1
	KindD2
	KindNomnoml
	KindSVG
)

var kindNames = map[Kind]string{KindMermaid: "mermaid", KindD2: "d2", KindNomnoml: "nomnoml", KindSVG: "svg"}

func (k Kind) String() string { return kindNames[k] }

func ParseKind(lang string) (Kind, bool) {
	for k, name := range kindNames {
		if name == lang {
			return k, true
		}
	}
	return 0, false
}

const labelXHeight = 0.486

type engines struct {
	mermaid  *quickjs.VM
	nomnoml  *quickjs.VM
	formulas formulaSource
}

func newEngines(fonts Fonts) *engines {
	return &engines{formulas: fonts.formulas()}
}

func (e *engines) script(kind Kind) (*quickjs.VM, error) {
	slot, load := &e.mermaid, newMermaidVM
	if kind == KindNomnoml {
		slot, load = &e.nomnoml, newNomnomlVM
	}
	if *slot != nil {
		return *slot, nil
	}
	vm, err := load()
	if err != nil {
		return nil, err
	}
	*slot = vm
	return vm, nil
}

func (e *engines) close() {
	for _, vm := range [...]*quickjs.VM{e.mermaid, e.nomnoml} {
		if vm != nil {
			_ = vm.Close()
		}
	}
}

type figureRequest struct {
	kind   Kind
	source string
	theme  Theme
	scale  float64
}

func (e *engines) render(ctx context.Context, r figureRequest) (*List, error) {
	if len(r.source) > SourceLimit {
		return nil, fail(FailureLimit, "figure source has %d bytes; the limit is %d", len(r.source), SourceLimit)
	}
	out, profile, err := e.engineSVG(ctx, r)
	if err != nil {
		return nil, err
	}
	if err := ctx.Err(); err != nil {
		return nil, fail(FailureTimeout, "figure stopped at its deadline")
	}
	return DecodeSVG(out.svg, DecodeOptions{Profile: profile, Scale: r.scale, XHeight: labelXHeight, overlays: out.overlays})
}

func (e *engines) engineSVG(ctx context.Context, r figureRequest) (engineOutput, Profile, error) {
	switch r.kind {
	case KindSVG:
		return engineOutput{svg: []byte(r.source)}, ProfileSVG, nil
	case KindD2:
		out, err := renderD2(ctx, d2Request{source: r.source, theme: r.theme, formulas: e.formulas})
		return out, ProfileD2, err
	case KindMermaid, KindNomnoml:
		return e.scriptSVG(ctx, r)
	}
	return engineOutput{}, 0, fail(FailureInvalid, "figure kind %d is not registered", r.kind)
}

func (e *engines) scriptSVG(ctx context.Context, r figureRequest) (engineOutput, Profile, error) {
	vm, err := e.script(r.kind)
	if err != nil {
		return engineOutput{}, 0, err
	}
	render := renderMermaid
	profile := ProfileMermaid
	if r.kind == KindNomnoml {
		render, profile = renderNomnoml, ProfileNomnoml
	}
	svg, err := render(ctx, vm, r.source, r.theme)
	return engineOutput{svg: svg}, profile, err
}
