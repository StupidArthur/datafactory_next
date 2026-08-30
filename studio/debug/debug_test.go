package debug

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"testing"
)

func TestMain(m *testing.M) {
	if os.Getenv("DF_HELPER_PROCESS") == "1" {
		helperProcess()
		return
	}
	os.Exit(m.Run())
}

type fake struct {
	stopped bool
	n       uint64
}

func (f *fake) Get(name string) (any, error) { return map[string]any{"name": name}, nil }

func (f *fake) Execute(n uint64, _ map[string]any) (Snapshot, error) {
	f.n += n
	return Snapshot{CycleIndex: f.n}, nil
}
func (f *fake) Simulate(n, samples uint64) ([]Snapshot, error) {
	return []Snapshot{{CycleIndex: n}, {CycleIndex: samples}}, nil
}
func (f *fake) Generate(uint64, uint64, string) error { return nil }
func (f *fake) Stop() error                           { f.stopped = true; return nil }
func TestManager(t *testing.T) {
	f := &fake{}
	m := New(func(string) (Engine, error) { return f, nil })
	j, e := m.Start("a")
	if e != nil {
		t.Fatal(e)
	}
	s, e := m.Execute(j.ID, 2, nil)
	if e != nil || s.CycleIndex != 2 {
		t.Fatal(s, e)
	}
	if x, _ := m.Trend(j.ID, 0); len(x) != 1 {
		t.Fatal(x)
	}
	if e = m.Stop(j.ID); e != nil || !f.stopped {
		t.Fatal(e)
	}
	if _, e = m.Execute(j.ID, 1, nil); e == nil {
		t.Fatal("stopped")
	}
	if _, e = m.Trend(j.ID, -1); e == nil {
		t.Fatal("offset")
	}
}
func TestManagerErrors(t *testing.T) {
	m := New(func(string) (Engine, error) { return nil, testingErr{} })
	if _, e := m.Start(""); e == nil {
		t.Fatal("required")
	}
	if _, e := m.Start("a"); e == nil {
		t.Fatal("factory")
	}
	m = New(func(string) (Engine, error) { return &fake{}, nil })
	if _, e := m.Execute("x", 1, nil); e == nil {
		t.Fatal("job")
	}
	if e := m.Stop("x"); e == nil {
		t.Fatal("job")
	}
}

type testingErr struct{}

func (testingErr) Error() string { return "x" }

func TestProcessEngine(t *testing.T) {
	if err := os.Setenv("DF_HELPER_PROCESS", "1"); err != nil {
		t.Fatal(err)
	}
	defer os.Unsetenv("DF_HELPER_PROCESS")
	p, err := StartProcess(os.Args[0], "artifact")
	if err != nil {
		t.Fatal(err)
	}
	s, err := p.Execute(2, map[string]any{"x": 1})
	if err != nil || s.CycleIndex != 2 {
		t.Fatalf("snapshot=%v err=%v", s, err)
	}
	samples, err := p.Simulate(10, 2)
	if err != nil || len(samples) != 2 {
		t.Fatalf("samples=%v err=%v", samples, err)
	}
	if err := p.Generate(10, 1, "result.parquet"); err != nil {
		t.Fatal(err)
	}
	if err := p.Stop(); err != nil {
		t.Fatal(err)
	}
}

func TestHelperProcess(t *testing.T) {
	if os.Getenv("DF_HELPER_PROCESS") != "1" {
		return
	}
	helperProcess()
}

func helperProcess() {
	in := bufio.NewScanner(os.Stdin)
	for in.Scan() {
		var req map[string]any
		if json.Unmarshal(in.Bytes(), &req) != nil {
			fmt.Fprintln(os.Stdout, `{ "code": 1302, "message": "bad" }`)
			continue
		}
		if req["operation"] == "stop" {
			fmt.Fprintln(os.Stdout, `{ "code": 0 }`)
			os.Exit(0)
		}
		if req["operation"] == "get" {
			fmt.Fprintln(os.Stdout, `{ "code": 0, "content": { "name": "ok" } }`)
			continue
		}
		if req["operation"] == "simulate" {
			fmt.Fprintln(os.Stdout, `{ "code": 0, "snapshots": [{"cycle_index": 5,"simulation_time": 1,"values":{}},{"cycle_index":10,"simulation_time":2,"values":{}}] }`)
			continue
		}
		if req["operation"] == "generate" {
			fmt.Fprintln(os.Stdout, `{ "code": 0 }`)
			continue
		}
		fmt.Fprintln(os.Stdout, `{ "code": 0, "snapshot": { "cycle_index": 2, "simulation_time": 1, "values": {} } }`)
	}
	os.Exit(0)
}
