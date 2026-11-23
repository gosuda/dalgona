package rpc

import (
	"encoding/json/jsontext"
)

type SettingsReadResponse struct {
	Settings []Setting
}

type Setting struct {
	Key     string
	Value   any
	Default any
	Options []any
}

func (r SettingsReadResponse) Encode(o *Writer) {
	o.Open()
	o.Key("settings")
	o.OpenArray()
	for _, setting := range r.Settings {
		setting.Encode(o)
	}
	o.CloseArray()
	o.Close()
}

func (s Setting) Encode(o *Writer) {
	o.Open()
	o.Str("key", s.Key)
	o.Key("value")
	writeSettingValue(o, s.Value)
	o.Key("defaultValue")
	writeSettingValue(o, s.Default)
	o.Key("options")
	o.OpenArray()
	for _, option := range s.Options {
		writeSettingValue(o, option)
	}
	o.CloseArray()
	o.Close()
}

func writeSettingValue(o *Writer, value any) {
	switch typed := value.(type) {
	case string:
		o.Text(typed)
	case bool:
		o.Token(jsontext.Bool(typed))
	case []string:
		o.OpenArray()
		for _, item := range typed {
			o.Text(item)
		}
		o.CloseArray()
	default:
		o.Null()
	}
}
