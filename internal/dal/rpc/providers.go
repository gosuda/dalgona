package rpc

type AccountProvidersResponse struct {
	Providers []AccountProvider `json:"providers"`
}

type AccountProvider struct {
	ID       string `json:"id"`
	Login    string `json:"login"`
	SignedIn bool   `json:"signedIn"`
}

func (r AccountProvidersResponse) Encode(o *Writer) {
	o.Open()
	o.Key("providers")
	o.OpenArray()
	for _, provider := range r.Providers {
		provider.Encode(o)
	}
	o.CloseArray()
	o.Close()
}

func (p AccountProvider) Encode(o *Writer) {
	o.Open()
	o.Str("id", p.ID)
	o.Str("login", p.Login)
	o.Bool("signedIn", p.SignedIn)
	o.Close()
}
