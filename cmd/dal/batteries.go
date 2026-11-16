package main

import (
	"context"

	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/scripthost"
)

func evalBatteries(_ context.Context, base app.Base) (app.Batteries, error) {
	return app.Batteries{Extension: scripthost.New(scripthost.Config{Judge: base.Judge, Mode: base.Config.ToolMode, Eval: base.Config.Eval, Logger: base.Logger})}, nil
}
