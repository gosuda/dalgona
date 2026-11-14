package script

import (
	"fmt"
	"slices"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

const EvalName = "eval"

const ratchetName = "ratchet"

var evalFirstNames = []string{"read", "search", "patch", "apply_patch", "exec", asktool.Name, ratchetName, tool.YieldName, EvalName}

func visibleNames(mode config.ToolMode) []string {
	switch mode {
	case config.ToolModeNormal:
		return nil
	case config.ToolModeEvalFirst:
		return evalFirstNames
	case config.ToolModeEvalOnly:
		return []string{asktool.Name, tool.YieldName, EvalName}
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

func inCells(name string) bool {
	return name != EvalName && name != tool.YieldName
}

func waitsForUser(name, action string) bool {
	return name == asktool.Name || (name == ratchetName && action == "approve")
}

func CheckDirect(mode config.ToolMode, name string) error {
	names := visibleNames(mode)
	if names == nil || slices.Contains(names, name) {
		return nil
	}
	return fmt.Errorf("tool %s is not available in tool_mode %s; call it from eval", name, mode)
}
