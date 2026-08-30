package application

import (
	"context"
	"encoding/csv"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"sync"

	"datafactory-next/studio/compiler"
	"datafactory-next/studio/debug"
	"datafactory-next/studio/exporter"
	"datafactory-next/studio/files"
	wails "github.com/wailsapp/wails/v2/pkg/runtime"
)

type Features struct {
	Metadata          any `json:"metadata"`
	Tags              any `json:"tags"`
	DefaultTrends     any `json:"defaultTrends"`
	DefaultExportTags any `json:"defaultExportTags"`
}
type RunResult struct {
	Job      debug.Job `json:"job"`
	Features Features  `json:"features"`
}
type SimulationResult struct {
	Features  Features         `json:"features"`
	Snapshots []debug.Snapshot `json:"snapshots"`
}
type ExportOptions struct {
	FileType  string   `json:"fileType"`
	TitleRows int      `json:"titleRows"`
	Features  []string `json:"features"`
	StartTime *float64 `json:"startTime"`
	EndTime   *float64 `json:"endTime"`
}

type App struct {
	ctx        context.Context
	enginePath string
	workspace  *files.Workspace
	manager    *debug.Manager
	activeJob  string
	artifact   string
	mu         sync.Mutex
}

func New(enginePath string) *App {
	return &App{enginePath: enginePath, manager: debug.New(func(artifact string) (debug.Engine, error) { return debug.StartProcess(enginePath, artifact) })}
}

func (a *App) Startup(ctx context.Context) { a.ctx = ctx }
func (a *App) Shutdown(context.Context) {
	a.mu.Lock()
	id, manager, artifact := a.activeJob, a.manager, a.artifact
	a.mu.Unlock()
	if id != "" {
		_ = manager.Stop(id)
	}
	if artifact != "" {
		_ = os.RemoveAll(filepath.Dir(artifact))
	}
}

func (a *App) SelectWorkspace() (string, error) {
	path, err := wails.OpenDirectoryDialog(a.ctx, wails.OpenDialogOptions{Title: "选择 DSL 工作目录"})
	if err != nil || path == "" {
		return path, err
	}
	return path, a.SetWorkspace(path)
}
func (a *App) SetWorkspace(path string) error {
	w, err := files.Open(path)
	if err != nil {
		return err
	}
	a.mu.Lock()
	a.workspace = &w
	a.mu.Unlock()
	return nil
}
func (a *App) ListDSL() ([]files.File, error) {
	a.mu.Lock()
	w := a.workspace
	a.mu.Unlock()
	if w == nil {
		return nil, fmt.Errorf("workspace is not selected")
	}
	return w.ListDSL()
}

func (a *App) compile(source string) (string, error) {
	a.mu.Lock()
	w := a.workspace
	a.mu.Unlock()
	if w == nil {
		return "", fmt.Errorf("workspace is not selected")
	}
	if _, err := w.Read(source); err != nil {
		return "", err
	}
	dir, err := os.MkdirTemp("", "datafactory-studio-")
	if err != nil {
		return "", err
	}
	artifact := filepath.Join(dir, "debug.dfproject")
	_, err = compiler.New(a.enginePath).Compile(context.Background(), source, artifact)
	if err != nil {
		_ = os.RemoveAll(dir)
		return "", err
	}
	return artifact, nil
}

func (a *App) Inspect(source string) (Features, error) {
	artifact, err := a.compile(source)
	if err != nil {
		return Features{}, err
	}
	defer os.RemoveAll(filepath.Dir(artifact))
	e, err := debug.StartProcess(a.enginePath, artifact)
	if err != nil {
		return Features{}, err
	}
	defer e.Stop()
	metadata, err := e.Get("project_metadata")
	if err != nil {
		return Features{}, err
	}
	tags, err := e.Get("tag_schema")
	if err != nil {
		return Features{}, err
	}
	defaultTrends, err := e.Get("default_trends")
	if err != nil {
		return Features{}, err
	}
	defaultExportTags, err := e.Get("default_export_tags")
	if err != nil {
		return Features{}, err
	}
	return Features{Metadata: metadata, Tags: tags, DefaultTrends: defaultTrends, DefaultExportTags: defaultExportTags}, nil
}

func (a *App) Simulate(source string, cycles, sampleCount uint64) (SimulationResult, error) {
	artifact, err := a.compile(source)
	if err != nil {
		return SimulationResult{}, err
	}
	defer os.RemoveAll(filepath.Dir(artifact))
	e, err := debug.StartProcess(a.enginePath, artifact)
	if err != nil {
		return SimulationResult{}, err
	}
	defer e.Stop()
	metadata, err := e.Get("project_metadata")
	if err != nil {
		return SimulationResult{}, err
	}
	tags, err := e.Get("tag_schema")
	if err != nil {
		return SimulationResult{}, err
	}
	defaultTrends, err := e.Get("default_trends")
	if err != nil {
		return SimulationResult{}, err
	}
	defaultExportTags, err := e.Get("default_export_tags")
	if err != nil {
		return SimulationResult{}, err
	}
	snapshots, err := e.Simulate(cycles, sampleCount)
	if err != nil {
		return SimulationResult{}, err
	}
	return SimulationResult{Features: Features{Metadata: metadata, Tags: tags, DefaultTrends: defaultTrends, DefaultExportTags: defaultExportTags}, Snapshots: snapshots}, nil
}

func (a *App) GenerateData(source string, cycles, sampleInterval uint64, options ExportOptions) (string, error) {
	if options.FileType != "csv" {
		return "", fmt.Errorf("unsupported export type: %s", options.FileType)
	}
	destination, err := wails.SaveFileDialog(a.ctx, wails.SaveDialogOptions{Title: "生成仿真数据", DefaultFilename: "simulation.csv", Filters: []wails.FileFilter{{DisplayName: "CSV 文件", Pattern: "*.csv"}}})
	if err != nil || destination == "" {
		return destination, err
	}
	artifact, err := a.compile(source)
	if err != nil {
		return "", err
	}
	defer os.RemoveAll(filepath.Dir(artifact))
	e, err := debug.StartProcess(a.enginePath, artifact)
	if err != nil {
		return "", err
	}
	defer e.Stop()
	temporaryDir, err := os.MkdirTemp("", "datafactory-export-")
	if err != nil {
		return "", err
	}
	defer os.RemoveAll(temporaryDir)
	parquetPath := filepath.Join(temporaryDir, "simulation.parquet")
	if err := e.Generate(cycles, sampleInterval, parquetPath); err != nil {
		return "", err
	}
	if err := exporter.ParquetToCSV(parquetPath, destination, exporter.CSVOptions{TitleRows: options.TitleRows, Features: options.Features, StartTime: options.StartTime, EndTime: options.EndTime}); err != nil {
		_ = os.Remove(destination)
		return "", err
	}
	return destination, nil
}

func (a *App) StartDebug(source string) (RunResult, error) {
	a.mu.Lock()
	previous, previousManager, previousArtifact := a.activeJob, a.manager, a.artifact
	a.mu.Unlock()
	if previous != "" {
		_ = previousManager.Stop(previous)
	}
	if previousArtifact != "" {
		_ = os.RemoveAll(filepath.Dir(previousArtifact))
	}
	artifact, err := a.compile(source)
	if err != nil {
		return RunResult{}, err
	}
	e, err := debug.StartProcess(a.enginePath, artifact)
	if err != nil {
		_ = os.RemoveAll(filepath.Dir(artifact))
		return RunResult{}, err
	}
	metadata, err := e.Get("project_metadata")
	if err != nil {
		_ = e.Stop()
		return RunResult{}, err
	}
	tags, err := e.Get("tag_schema")
	if err != nil {
		_ = e.Stop()
		return RunResult{}, err
	}
	defaultTrends, err := e.Get("default_trends")
	if err != nil {
		_ = e.Stop()
		return RunResult{}, err
	}
	defaultExportTags, err := e.Get("default_export_tags")
	if err != nil {
		_ = e.Stop()
		return RunResult{}, err
	}
	m := debug.New(func(string) (debug.Engine, error) { return e, nil })
	job, err := m.Start(artifact)
	if err != nil {
		return RunResult{}, err
	}
	a.mu.Lock()
	a.manager = m
	a.activeJob = job.ID
	a.artifact = artifact
	a.mu.Unlock()
	return RunResult{Job: job, Features: Features{Metadata: metadata, Tags: tags, DefaultTrends: defaultTrends, DefaultExportTags: defaultExportTags}}, nil
}
func (a *App) Execute(jobID string, cycles uint64, writes map[string]any) ([]debug.Snapshot, error) {
	a.mu.Lock()
	m := a.manager
	a.mu.Unlock()
	if cycles == 0 {
		s, err := m.Execute(jobID, 0, writes)
		if err != nil {
			return nil, err
		}
		return []debug.Snapshot{s}, nil
	}
	result := make([]debug.Snapshot, 0, cycles)
	for i := uint64(0); i < cycles; i++ {
		currentWrites := map[string]any{}
		if i == 0 {
			currentWrites = writes
		}
		s, err := m.Execute(jobID, 1, currentWrites)
		if err != nil {
			return nil, err
		}
		result = append(result, s)
	}
	return result, nil
}
func (a *App) Trend(jobID string, offset int) ([]debug.Snapshot, error) {
	a.mu.Lock()
	m := a.manager
	a.mu.Unlock()
	return m.Trend(jobID, offset)
}
func (a *App) StopDebug(jobID string) error {
	a.mu.Lock()
	m := a.manager
	a.mu.Unlock()
	err := m.Stop(jobID)
	a.mu.Lock()
	artifact := a.artifact
	if a.activeJob == jobID {
		a.activeJob, a.artifact = "", ""
	}
	a.mu.Unlock()
	if artifact != "" {
		_ = os.RemoveAll(filepath.Dir(artifact))
	}
	return err
}

func (a *App) Export(jobID string, options ExportOptions) (string, error) {
	rows, err := a.Trend(jobID, 0)
	if err != nil {
		return "", err
	}
	if options.FileType != "csv" {
		return "", fmt.Errorf("unsupported export type: %s", options.FileType)
	}
	filtered := make([]debug.Snapshot, 0, len(rows))
	for _, row := range rows {
		if options.StartTime != nil && row.SimulationTime < *options.StartTime {
			continue
		}
		if options.EndTime != nil && row.SimulationTime > *options.EndTime {
			continue
		}
		filtered = append(filtered, row)
	}
	path, err := wails.SaveFileDialog(a.ctx, wails.SaveDialogOptions{Title: "导出调试数据", DefaultFilename: "studio-data.csv", Filters: []wails.FileFilter{{DisplayName: "CSV 文件", Pattern: "*.csv"}}})
	if err != nil || path == "" {
		return path, err
	}
	f, err := os.Create(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	keys := append([]string(nil), options.Features...)
	if len(keys) == 0 && len(filtered) > 0 {
		for key := range filtered[0].Values {
			keys = append(keys, key)
		}
		sort.Strings(keys)
	}
	writer := csv.NewWriter(f)
	if options.TitleRows > 1 {
		if err = writer.Write([]string{"DataFactory Studio 调试数据"}); err != nil {
			return "", err
		}
	}
	if options.TitleRows > 0 {
		if err = writer.Write(append([]string{"cycle_index", "simulation_time"}, keys...)); err != nil {
			return "", err
		}
	}
	for _, row := range filtered {
		record := []string{strconv.FormatUint(row.CycleIndex, 10), strconv.FormatFloat(row.SimulationTime, 'f', -1, 64)}
		for _, key := range keys {
			record = append(record, fmt.Sprint(row.Values[key]))
		}
		if err = writer.Write(record); err != nil {
			return "", err
		}
	}
	writer.Flush()
	return path, writer.Error()
}

func (a *App) ExportCSV(jobID string) (string, error) {
	rows, err := a.Trend(jobID, 0)
	if err != nil {
		return "", err
	}
	path, err := wails.SaveFileDialog(a.ctx, wails.SaveDialogOptions{Title: "导出调试数据", DefaultFilename: "studio-data.csv", Filters: []wails.FileFilter{{DisplayName: "CSV 文件", Pattern: "*.csv"}}})
	if err != nil || path == "" {
		return path, err
	}
	f, err := os.Create(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	keys := []string{}
	if len(rows) > 0 {
		for k := range rows[0].Values {
			keys = append(keys, k)
		}
		sort.Strings(keys)
	}
	writer := csv.NewWriter(f)
	header := append([]string{"cycle_index", "simulation_time"}, keys...)
	if err = writer.Write(header); err != nil {
		return "", err
	}
	for _, row := range rows {
		record := []string{strconv.FormatUint(row.CycleIndex, 10), strconv.FormatFloat(row.SimulationTime, 'f', -1, 64)}
		for _, key := range keys {
			record = append(record, fmt.Sprint(row.Values[key]))
		}
		if err = writer.Write(record); err != nil {
			return "", err
		}
	}
	writer.Flush()
	return path, writer.Error()
}
