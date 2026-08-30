package debug

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os/exec"
	"sync"

	"datafactory-next/studio/internal/childprocess"
)

type Snapshot struct {
	CycleIndex     uint64         `json:"cycle_index"`
	SimulationTime float64        `json:"simulation_time"`
	Values         map[string]any `json:"values"`
}
type Engine interface {
	Execute(uint64, map[string]any) (Snapshot, error)
	Simulate(uint64, uint64) ([]Snapshot, error)
	Generate(uint64, uint64, string) error
	Get(string) (any, error)
	Stop() error
}
type Factory func(string) (Engine, error)
type Job struct {
	ID       string     `json:"id"`
	Artifact string     `json:"artifact"`
	Running  bool       `json:"running"`
	Trend    []Snapshot `json:"-"`
}
type Manager struct {
	mu      sync.Mutex
	factory Factory
	jobs    map[string]*Job
	engines map[string]Engine
	next    uint64
}

func New(factory Factory) *Manager {
	return &Manager{factory: factory, jobs: map[string]*Job{}, engines: map[string]Engine{}}
}
func (m *Manager) Start(artifact string) (Job, error) {
	if artifact == "" {
		return Job{}, fmt.Errorf("artifact is required")
	}
	e, err := m.factory(artifact)
	if err != nil {
		return Job{}, err
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	m.next++
	id := fmt.Sprintf("job-%d", m.next)
	j := &Job{ID: id, Artifact: artifact, Running: true, Trend: []Snapshot{}}
	m.jobs[id] = j
	m.engines[id] = e
	return *j, nil
}
func (m *Manager) Execute(id string, cycles uint64, writes map[string]any) (Snapshot, error) {
	m.mu.Lock()
	j, ok := m.jobs[id]
	e := m.engines[id]
	m.mu.Unlock()
	if !ok {
		return Snapshot{}, fmt.Errorf("job not found")
	}
	if !j.Running {
		return Snapshot{}, fmt.Errorf("job is stopped")
	}
	s, err := e.Execute(cycles, writes)
	if err != nil {
		return Snapshot{}, err
	}
	m.mu.Lock()
	j.Trend = append(j.Trend, s)
	m.mu.Unlock()
	return s, nil
}
func (m *Manager) Trend(id string, offset int) ([]Snapshot, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	j, ok := m.jobs[id]
	if !ok {
		return nil, fmt.Errorf("job not found")
	}
	if offset < 0 || offset > len(j.Trend) {
		return nil, fmt.Errorf("invalid offset")
	}
	out := append([]Snapshot(nil), j.Trend[offset:]...)
	return out, nil
}
func (m *Manager) Stop(id string) error {
	m.mu.Lock()
	j, ok := m.jobs[id]
	e := m.engines[id]
	if ok {
		j.Running = false
	}
	m.mu.Unlock()
	if !ok {
		return fmt.Errorf("job not found")
	}
	return e.Stop()
}

type processEngine struct {
	cmd *exec.Cmd
	in  io.WriteCloser
	out *bufio.Reader
	mu  sync.Mutex
	seq uint64
}

func StartProcess(enginePath, artifact string) (Engine, error) {
	if enginePath == "" || artifact == "" {
		return nil, fmt.Errorf("engine path and artifact are required")
	}
	c := exec.Command(enginePath, "run", artifact)
	childprocess.HideWindow(c)
	in, err := c.StdinPipe()
	if err != nil {
		return nil, err
	}
	out, err := c.StdoutPipe()
	if err != nil {
		return nil, err
	}
	if err := c.Start(); err != nil {
		return nil, err
	}
	return &processEngine{cmd: c, in: in, out: bufio.NewReader(out)}, nil
}
func (p *processEngine) call(op string, payload any, dst any) error {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.seq++
	req := map[string]any{"id": p.seq, "operation": op}
	if payload != nil {
		for k, v := range payload.(map[string]any) {
			req[k] = v
		}
	}
	b, _ := json.Marshal(req)
	if _, err := fmt.Fprintf(p.in, "%s\n", b); err != nil {
		return err
	}
	line, err := p.out.ReadBytes('\n')
	if err != nil {
		return err
	}
	var r struct {
		Code      int             `json:"code"`
		Message   string          `json:"message"`
		Snapshot  json.RawMessage `json:"snapshot"`
		Snapshots json.RawMessage `json:"snapshots"`
		Content   json.RawMessage `json:"content"`
	}
	if err := json.Unmarshal(line, &r); err != nil {
		return err
	}
	if r.Code != 0 {
		return fmt.Errorf("engine %d: %s", r.Code, r.Message)
	}
	if dst != nil {
		body := r.Snapshot
		if op == "get" {
			body = r.Content
		} else if op == "simulate" {
			body = r.Snapshots
		}
		return json.Unmarshal(body, dst)
	}
	return nil
}
func (p *processEngine) Simulate(cycles, sampleCount uint64) ([]Snapshot, error) {
	var snapshots []Snapshot
	err := p.call("simulate", map[string]any{"cycles": cycles, "sample_count": sampleCount}, &snapshots)
	return snapshots, err
}
func (p *processEngine) Generate(cycles, sampleInterval uint64, filePath string) error {
	return p.call("generate", map[string]any{"cycles": cycles, "sample_interval": sampleInterval, "file_path": filePath}, nil)
}
func (p *processEngine) Execute(c uint64, w map[string]any) (Snapshot, error) {
	var s Snapshot
	err := p.call("execute", map[string]any{"cycles": c, "writes": w}, &s)
	return s, err
}
func (p *processEngine) Get(name string) (any, error) {
	var content any
	err := p.call("get", map[string]any{"name": name}, &content)
	return content, err
}
func (p *processEngine) Stop() error {
	p.mu.Lock()
	defer p.mu.Unlock()
	_, err := fmt.Fprintln(p.in, `{"id":0,"operation":"stop"}`)
	if err != nil {
		return err
	}
	_ = p.in.Close()
	return p.cmd.Wait()
}
