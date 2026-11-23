package rpc

type CommandExecResponse struct {
	ExitCode int    `json:"exitCode"`
	Stdout   string `json:"stdout"`
	Stderr   string `json:"stderr"`
}

func (r CommandExecResponse) Encode(o *Writer) {
	o.Open()
	o.Int("exitCode", int64(r.ExitCode))
	o.Str("stderr", r.Stderr)
	o.Str("stdout", r.Stdout)
	o.Close()
}
