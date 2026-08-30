package debug

import (
	"encoding/csv"
	"os"
	"path/filepath"
	"testing"

	"datafactory-next/studio/exporter"
)

func TestRealEngineIntegration(t *testing.T) {
	engine := os.Getenv("DF_ENGINE_PATH")
	project := os.Getenv("DF_PROJECT_PATH")
	if engine == "" || project == "" {
		t.Skip("set DF_ENGINE_PATH and DF_PROJECT_PATH for the real-engine check")
	}
	if _, err := os.Stat(filepath.Clean(engine)); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Clean(project)); err != nil {
		t.Fatal(err)
	}
	e, err := StartProcess(engine, project)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := e.Get("project_metadata"); err != nil {
		t.Fatal(err)
	}
	s, err := e.Execute(1, map[string]any{"plant_inlet_flow": 0.0015})
	if err != nil {
		t.Fatal(err)
	}
	if s.CycleIndex != 1 {
		t.Fatalf("cycle=%d", s.CycleIndex)
	}
	samples, err := e.Simulate(100, 10)
	if err != nil || len(samples) != 10 || samples[9].CycleIndex != 100 {
		t.Fatalf("samples=%v err=%v", samples, err)
	}
	output := filepath.Join(t.TempDir(), "data.parquet")
	if err := e.Generate(100, 2, output); err != nil {
		t.Fatal(err)
	}
	if info, err := os.Stat(output); err != nil || info.Size() == 0 {
		t.Fatalf("output info=%v err=%v", info, err)
	}
	csvPath := filepath.Join(t.TempDir(), "data.csv")
	if err := exporter.ParquetToCSV(output, csvPath, exporter.CSVOptions{TitleRows: 1}); err != nil {
		t.Fatal(err)
	}
	csvFile, err := os.Open(csvPath)
	if err != nil {
		t.Fatal(err)
	}
	records, err := csv.NewReader(csvFile).ReadAll()
	_ = csvFile.Close()
	if err != nil || len(records) != 51 {
		t.Fatalf("csv rows=%d err=%v", len(records), err)
	}
	if err := e.Stop(); err != nil {
		t.Fatal(err)
	}
}
