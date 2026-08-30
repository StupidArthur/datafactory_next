package application

import (
	"os"
	"path/filepath"
	"testing"
)

func TestStudioRealEngineWorkflow(t *testing.T) {
	engine := os.Getenv("DF_ENGINE_PATH")
	workspace := os.Getenv("DF_STUDIO_WORKSPACE")
	if engine == "" || workspace == "" {
		t.Skip("set real-engine integration environment")
	}
	app := New(engine)
	if err := app.SetWorkspace(workspace); err != nil {
		t.Fatal(err)
	}
	list, err := app.ListDSL()
	if err != nil || len(list) == 0 {
		t.Fatalf("files=%v err=%v", list, err)
	}
	source := filepath.Join(workspace, "project.yaml")
	if _, err := app.Inspect(source); err != nil {
		t.Fatal(err)
	}
	simulation, err := app.Simulate(source, 2_000_000, 1200)
	if err != nil || len(simulation.Snapshots) != 1200 || simulation.Snapshots[1199].CycleIndex != 2_000_000 {
		t.Fatalf("simulation rows=%d err=%v", len(simulation.Snapshots), err)
	}
	run, err := app.StartDebug(source)
	if err != nil {
		t.Fatal(err)
	}
	rows, err := app.Execute(run.Job.ID, 3, map[string]any{"plant_inlet_flow": 0.0015})
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 3 || rows[2].CycleIndex != 3 {
		t.Fatalf("rows=%v", rows)
	}
	trend, err := app.Trend(run.Job.ID, 0)
	if err != nil || len(trend) != 3 {
		t.Fatalf("trend=%v err=%v", trend, err)
	}
	if err := app.StopDebug(run.Job.ID); err != nil {
		t.Fatal(err)
	}
}
