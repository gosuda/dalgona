package platform

import "testing"

func TestDialogRequestsFollowTheDialogContract(t *testing.T) {
	images := []FileFilter{{Name: "Images", Extensions: []string{"png", "jpeg"}}}
	valid := []DialogRequest{
		{Kind: DialogOpenFiles, Title: "Attach files", Filters: images},
		{Kind: DialogChooseFolder, Title: "Choose workspace folder"},
		{Kind: DialogSaveFile, Title: "Export document", SuggestedName: "notes.md", Filters: []FileFilter{{Name: "Markdown", Extensions: []string{"md"}}}},
	}
	for _, r := range valid {
		if err := r.Check(); err != nil {
			t.Errorf("%+v refused: %v", r, err)
		}
	}
	invalid := map[string]DialogRequest{
		"unknown kind":            {Kind: 0, Title: "x"},
		"untitled":                {Kind: DialogOpenFiles},
		"folder with filters":     {Kind: DialogChooseFolder, Title: "Choose", Filters: images},
		"save without a name":     {Kind: DialogSaveFile, Title: "Save"},
		"open with a name":        {Kind: DialogOpenFiles, Title: "Open", SuggestedName: "a.md"},
		"name with a separator":   {Kind: DialogSaveFile, Title: "Save", SuggestedName: "../a.md"},
		"filter without suffixes": {Kind: DialogOpenFiles, Title: "Open", Filters: []FileFilter{{Name: "Images"}}},
		"dotted suffix":           {Kind: DialogOpenFiles, Title: "Open", Filters: []FileFilter{{Name: "Images", Extensions: []string{".png"}}}},
		"wildcard suffix":         {Kind: DialogOpenFiles, Title: "Open", Filters: []FileFilter{{Name: "All", Extensions: []string{"*"}}}},
		"unnamed filter":          {Kind: DialogOpenFiles, Title: "Open", Filters: []FileFilter{{Extensions: []string{"png"}}}},
	}
	for name, r := range invalid {
		if err := r.Check(); err == nil {
			t.Errorf("%s: the request was accepted", name)
		}
	}
}
