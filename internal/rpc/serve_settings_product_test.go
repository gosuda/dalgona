package rpc

import (
	"reflect"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestSettingsReadRespectsProductBoundary(t *testing.T) {
	for _, test := range []struct {
		product config.Product
		keys    []string
	}{
		{
			product: config.ProductDal,
			keys:    []string{"approval", "verbosity", "theme", "icons", "statusHide", "notifications"},
		},
		{
			product: config.ProductDalgona,
			keys:    []string{"approval", "verbosity", "theme", "icons", "statusHide", "editReview", "notifications"},
		},
	} {
		t.Run(string(test.product), func(t *testing.T) {
			r := newRig(t, config.ApprovalAuto)
			r.cfg.Config.Product = test.product
			setDefaultSettings(t, r)
			c := r.connect()
			var result settingsReadResult
			c.ok("dalgona/settings/read", map[string]any{}, &result)
			keys := make([]string, 0, len(result.Settings))
			for _, setting := range result.Settings {
				keys = append(keys, setting.Key)
			}
			if !reflect.DeepEqual(keys, test.keys) {
				t.Fatalf("settings keys = %v, want %v", keys, test.keys)
			}
		})
	}
}
