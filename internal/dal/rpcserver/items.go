package rpcserver

import (
	"encoding/json/jsontext"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func toolKind(tool, class string) string {
	switch class {
	case "exec":
		return rpc.KindCommand
	case "edit":
		return rpc.KindFileChange
	case "remote":
		return rpc.KindMCP
	case "":
		return toolKindByName(tool)
	}
	return rpc.KindDynamic
}

func toolKindByName(tool string) string {
	switch tool {
	case "exec":
		return rpc.KindCommand
	case "patch", "apply_patch":
		return rpc.KindFileChange
	}
	if strings.Contains(tool, "__") {
		return rpc.KindMCP
	}
	return rpc.KindDynamic
}

func argString(args jsontext.Value, key string) string {
	var value string
	_ = decodeParams(args, func(name string, d *jsontext.Decoder) error {
		if name != key {
			return d.SkipValue()
		}
		v, err := rpc.ReadString(d)
		value = v
		return err
	})
	return value
}
