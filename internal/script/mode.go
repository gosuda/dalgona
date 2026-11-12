package script

import (
	"fmt"
	"slices"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

const EvalName = "eval"

var evalFirstNames = []string{"read", "search", "patch", "apply_patch", "exec", asktool.Name, EvalName}

func visibleNames(mode config.ToolMode) []string {
	switch mode {
	case config.ToolModeNormal:
		return nil
	case config.ToolModeEvalFirst:
		return evalFirstNames
	case config.ToolModeEvalOnly:
		return []string{asktool.Name, EvalName}
	}
	return nil
}

func ModelTools(mode config.ToolMode, all []tool.Spec) []tool.Spec {
	names := visibleNames(mode)
	if names == nil {
		return all
	}
	visible := make([]tool.Spec, 0, len(names))
	for _, name := range names {
		if i := slices.IndexFunc(all, func(spec tool.Spec) bool { return spec.Name == name }); i >= 0 {
			visible = append(visible, all[i])
		}
	}
	return visible
}

func FoldedTools(mode config.ToolMode, all []tool.Spec) []tool.Spec {
	names := visibleNames(mode)
	if names == nil {
		return nil
	}
	folded := make([]tool.Spec, 0, len(all))
	for _, spec := range all {
		if !slices.Contains(names, spec.Name) {
			folded = append(folded, spec)
		}
	}
	return folded
}

func DefaultUses(mode config.ToolMode, all []tool.Spec) []string {
	var source []tool.Spec
	switch mode {
	case config.ToolModeNormal:
		return []string{}
	case config.ToolModeEvalFirst:
		source = FoldedTools(mode, all)
	case config.ToolModeEvalOnly:
		source = all
	}
	uses := make([]string, 0, len(source))
	for _, spec := range source {
		if spec.Name != EvalName && spec.Name != asktool.Name {
			uses = append(uses, spec.Name)
		}
	}
	return uses
}

func CheckDirect(mode config.ToolMode, name string) error {
	names := visibleNames(mode)
	if names == nil || slices.Contains(names, name) {
		return nil
	}
	return fmt.Errorf("tool %s is not available in tool_mode %s; call it from eval", name, mode)
}
