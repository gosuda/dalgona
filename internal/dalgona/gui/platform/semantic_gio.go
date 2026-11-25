//go:build gui

package platform

import (
	"gioui.org/io/semantic"
	"gioui.org/op"
)

func (n SemanticNode) Add(ops *op.Ops) {
	semantic.LabelOp(n.Name).Add(ops)
	if d := n.description(); d != "" {
		semantic.DescriptionOp(d).Add(ops)
	}
	class := roleClass(n.Role)
	class.Add(ops)
	semantic.EnabledOp(!n.Disabled).Add(ops)
	if class == semantic.CheckBox || class == semantic.RadioButton || class == semantic.Switch {
		semantic.SelectedOp(n.Checked).Add(ops)
	}
}

func roleClass(r Role) semantic.ClassOp {
	switch r {
	case RoleButton, RoleLink, RoleMenuItem:
		return semantic.Button
	case RoleCheckBox:
		return semantic.CheckBox
	case RoleRadioButton:
		return semantic.RadioButton
	case RoleSwitch:
		return semantic.Switch
	case RoleTextField, RoleSecureField:
		return semantic.Editor
	case RoleDocument, RoleHeading, RoleList, RoleListItem, RoleTable, RoleRow, RoleColumnHeader, RoleRowHeader, RoleCell,
		RoleCode, RoleMath, RoleFigure, RoleImage, RoleTerminal, RoleDialog, RoleMenu, RoleStatus, RoleError:
	}
	return semantic.Unknown
}
