//go:build darwin || linux

package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
)

const configPath = "/api/v2/feature-flagging/config/rules-based/server"

type origin struct {
	mu        sync.Mutex
	server    *httptest.Server
	version   int
	calls     int
	status    int
	delay     time.Duration
	malformed bool
}

func newOrigin(t *testing.T) *origin {
	o := &origin{version: 1}
	o.server = httptest.NewServer(http.HandlerFunc(o.handle))
	t.Cleanup(o.server.Close)
	return o
}

func (o *origin) counts() int {
	o.mu.Lock()
	defer o.mu.Unlock()
	return o.calls
}

func (o *origin) set(fn func()) {
	o.mu.Lock()
	defer o.mu.Unlock()
	fn()
}

func (o *origin) handle(w http.ResponseWriter, r *http.Request) {
	o.mu.Lock()
	status, delay, malformed := o.status, o.delay, o.malformed
	o.calls++
	d := Document{Version: fmt.Sprintf("v%d", o.version), Configuration: fixture(o.version, r.URL.Query().Get("dd_env"))}
	o.mu.Unlock()
	if delay > 0 {
		select {
		case <-time.After(delay):
		case <-r.Context().Done():
			return
		}
	}
	if status != 0 {
		w.WriteHeader(status)
		return
	}
	etag := d.Version
	w.Header().Set("ETag", etag)
	if r.Header.Get("If-None-Match") == etag {
		w.WriteHeader(http.StatusNotModified)
		return
	}
	if malformed {
		_, _ = io.WriteString(w, "{invalid")
		return
	}
	_, _ = w.Write(d.Configuration)
}

func defaults(o *origin) Config {
	return Config{Mode: "cache", Upstream: o.server.URL + "/origin", TTL: time.Minute, MaxAge: 5 * time.Minute, Timeout: time.Second, FailureBackoff: time.Second}
}

// The test executable runs the same HTTP server in a separate OS process.
func TestProxyProcess(t *testing.T) {
	value := os.Getenv("LAMBDA_CACHE_PROTOTYPE_CONFIG")
	if value == "" {
		t.Skip("subprocess entry point")
	}
	var c Config
	if err := json.Unmarshal([]byte(value), &c); err != nil {
		os.Exit(2)
	}
	if serve(c, "127.0.0.1:0") != nil {
		os.Exit(3)
	}
}

type process struct {
	cmd  *exec.Cmd
	url  string
	mu   sync.Mutex
	end  bool
	exit chan error
}

func start(t *testing.T, c Config, environment ...string) *process {
	t.Helper()
	b, _ := json.Marshal(c)
	cmd := exec.Command(os.Args[0], "-test.run=^TestProxyProcess$")
	cmd.Env = append(os.Environ(), "AWS_LAMBDA_RUNTIME_API=", "LAMBDA_CACHE_PROTOTYPE_CONFIG="+string(b))
	cmd.Env = append(cmd.Env, environment...)
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	cmd.Stderr = os.Stderr
	if err = cmd.Start(); err != nil {
		t.Fatal(err)
	}
	p := &process{cmd: cmd, exit: make(chan error, 1)}
	t.Cleanup(p.stop)
	ready := make(chan string, 1)
	go func() {
		scanner := bufio.NewScanner(stdout)
		if scanner.Scan() {
			ready <- scanner.Text()
		}
		for scanner.Scan() {
		}
		p.exit <- cmd.Wait()
		close(p.exit)
	}()
	select {
	case line := <-ready:
		var payload map[string]string
		if json.Unmarshal([]byte(line), &payload) != nil || payload["url"] == "" {
			t.Fatalf("invalid process ready line: %q", line)
		}
		p.url = payload["url"]
	case <-time.After(5 * time.Second):
		t.Fatal("proxy did not start")
	}
	return p
}

func (p *process) stop() {
	p.mu.Lock()
	defer p.mu.Unlock()
	if !p.end {
		_ = p.cmd.Process.Signal(syscall.SIGCONT)
		_ = p.cmd.Process.Kill()
		<-p.exit
		p.end = true
	}
}

type readResult struct {
	status  int
	version string
	source  string
	flags   map[string]any
	elapsed time.Duration
	err     error
}

func read(base string, env string, budgetMS int) readResult {
	req, _ := http.NewRequest(http.MethodGet, base+configPath+"?dd_env="+env, nil)
	if budgetMS > 0 {
		req.Header.Set("X-Config-Budget-Ms", strconv.Itoa(budgetMS))
	}
	started := time.Now()
	resp, err := (&http.Client{Timeout: 3 * time.Second}).Do(req)
	if err != nil {
		return readResult{err: err}
	}
	defer resp.Body.Close()
	r := readResult{status: resp.StatusCode, version: resp.Header.Get("X-Config-Version"), source: resp.Header.Get("X-Config-Source")}
	if resp.StatusCode == http.StatusOK {
		r.err = json.NewDecoder(resp.Body).Decode(&r.flags)
	} else {
		_, _ = io.Copy(io.Discard, resp.Body)
	}
	r.elapsed = time.Since(started)
	return r
}

func requireVersion(t *testing.T, r readResult, version string) {
	t.Helper()
	if r.err != nil || r.status != 200 || r.version != version {
		t.Fatalf("want HTTP 200/%s, got %+v", version, r)
	}
}

func TestWarmReadsAndPassthroughNegativeControl(t *testing.T) {
	for _, mode := range []string{"passthrough", "cache"} {
		t.Run(mode, func(t *testing.T) {
			o := newOrigin(t)
			c := defaults(o)
			c.Mode = mode
			p := start(t, c)
			for i := 0; i < 100; i++ {
				requireVersion(t, read(p.url, "prod", 0), "v1")
			}
			calls := o.counts()
			want := 1
			if mode == "passthrough" {
				want = 100
			}
			if calls != want {
				t.Fatalf("upstream calls: want %d got %d", want, calls)
			}
			t.Logf("100 local configuration reads; mode=%s; CDN requests=%d", mode, calls)
		})
	}
}

func TestConcurrentMissesShareOneFetch(t *testing.T) {
	o := newOrigin(t)
	o.delay = 60 * time.Millisecond
	p := start(t, defaults(o))
	results := make(chan readResult, 32)
	for i := 0; i < 32; i++ {
		go func() { results <- read(p.url, "prod", 0) }()
	}
	for i := 0; i < 32; i++ {
		requireVersion(t, <-results, "v1")
	}
	calls := o.counts()
	if calls != 1 {
		t.Fatalf("want one concurrent refresh, got %d", calls)
	}
	t.Log("32 concurrent reads in one environment produced one CDN request")
}

func TestFreezeThawRefreshesOnFirstRead(t *testing.T) {
	o := newOrigin(t)
	c := defaults(o)
	c.TTL = 100 * time.Millisecond
	p := start(t, c)
	before := read(p.url, "prod", 0)
	requireVersion(t, before, "v1")
	pause(t, p.cmd.Process)
	o.set(func() { o.version = 2 })
	time.Sleep(180 * time.Millisecond)
	calls := o.counts()
	if calls != 1 {
		t.Fatalf("unexpected work while frozen: %d calls", calls)
	}
	if err := p.cmd.Process.Signal(syscall.SIGCONT); err != nil {
		t.Fatal(err)
	}
	after := read(p.url, "prod", 0)
	requireVersion(t, after, "v2")
	if !strings.Contains(string(mustJSON(after.flags)), `"value":true`) || !strings.Contains(string(mustJSON(before.flags)), `"value":false`) {
		t.Fatal("returned configuration did not change")
	}
	t.Logf("SIGSTOP/SIGCONT verified; first resumed read=v2; local refresh latency=%s", after.elapsed)
}

func pause(t *testing.T, process *os.Process) {
	t.Helper()
	if err := process.Signal(syscall.SIGSTOP); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = process.Signal(syscall.SIGCONT) })
	deadline := time.Now().Add(2 * time.Second)
	for {
		state, err := exec.Command("ps", "-o", "state=", "-p", strconv.Itoa(process.Pid)).Output()
		if err != nil {
			t.Fatal(err)
		}
		if strings.Contains(string(state), "T") {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("child did not stop: %s", state)
		}
		time.Sleep(time.Millisecond)
	}
}

func TestFreshEnvironmentsEachNeedBootstrap(t *testing.T) {
	o := newOrigin(t)
	for i := 0; i < 16; i++ {
		p := start(t, defaults(o))
		requireVersion(t, read(p.url, "prod", 0), "v1")
		p.stop()
	}
	calls := o.counts()
	if calls != 16 {
		t.Fatalf("want one initial fetch per environment, got %d", calls)
	}
	t.Log("16 independent cold cache processes produced 16 CDN requests")
}

func TestScopeIsolation(t *testing.T) {
	o := newOrigin(t)
	p := start(t, defaults(o))
	for _, env := range []string{"prod", "stage", "prod", "stage"} {
		r := read(p.url, env, 0)
		requireVersion(t, r, "v1")
		if !strings.Contains(string(mustJSON(r.flags)), `"name":"`+env+`"`) {
			t.Fatalf("cross-environment configuration: %+v", r)
		}
	}
	calls := o.counts()
	if calls != 2 {
		t.Fatalf("want 2 independently cached scopes, got %d", calls)
	}
}

func TestInvalidRefreshPreservesLastValidSnapshot(t *testing.T) {
	o := newOrigin(t)
	c := defaults(o)
	c.TTL = 0
	c.StateDir = t.TempDir()
	p := start(t, c)
	requireVersion(t, read(p.url, "prod", 0), "v1")
	o.set(func() { o.version, o.malformed = 2, true })
	r := read(p.url, "prod", 0)
	requireVersion(t, r, "v1")
	if r.source != "stale" {
		t.Fatalf("want explicit stale fallback, got %+v", r)
	}
	files, _ := os.ReadDir(c.StateDir)
	if len(files) != 1 {
		t.Fatalf("unexpected persistence files: %v", files)
	}
	data, _ := os.ReadFile(c.StateDir + "/" + files[0].Name())
	if !json.Valid(data) || strings.Contains(string(data), `"version":"v2"`) {
		t.Fatal("invalid refresh replaced accepted persisted state")
	}
	t.Log("invalid JSON refresh retained v1 in memory and on disk")
}

func TestFailureBackoffPreventsSequentialRetryStorm(t *testing.T) {
	o := newOrigin(t)
	c := defaults(o)
	c.TTL = 0
	p := start(t, c)
	requireVersion(t, read(p.url, "prod", 0), "v1")
	o.set(func() { o.status = 503 })
	for i := 0; i < 20; i++ {
		r := read(p.url, "prod", 0)
		requireVersion(t, r, "v1")
		if r.source != "stale" {
			t.Fatal("failure must be visible as stale")
		}
	}
	calls := o.counts()
	if calls != 2 {
		t.Fatalf("want initial fetch and one failed refresh, got %d", calls)
	}
	t.Log("20 reads during outage caused one failed refresh, not 20 retries")
}

func TestBudgetBoundsColdStartAndRetainsWarmFallback(t *testing.T) {
	o := newOrigin(t)
	c := defaults(o)
	c.TTL = 0
	warm := start(t, c)
	requireVersion(t, read(warm.url, "prod", 0), "v1")
	o.set(func() { o.delay = 500 * time.Millisecond })
	r := read(warm.url, "prod", 50)
	requireVersion(t, r, "v1")
	if r.source != "stale" || r.elapsed > 250*time.Millisecond {
		t.Fatalf("warm budget/fallback failed: %+v", r)
	}
	cold := start(t, c)
	r2 := read(cold.url, "prod", 50)
	if r2.status != 503 || r2.elapsed > 250*time.Millisecond {
		t.Fatalf("cold budget failed: %+v", r2)
	}
	t.Logf("500ms origin delay / 50ms budget: warm=%s (cached v1), cold=%s (503, caller must use its default)", r.elapsed, r2.elapsed)
}

func TestEnvironmentLocalDiskSurvivesProcessResetOnly(t *testing.T) {
	o := newOrigin(t)
	c := defaults(o)
	c.StateDir = t.TempDir()
	first := start(t, c)
	requireVersion(t, read(first.url, "prod", 0), "v1")
	first.stop()
	reset := start(t, c)
	r := read(reset.url, "prod", 0)
	requireVersion(t, r, "v1")
	if r.source != "disk" {
		t.Fatalf("reset lost valid environment-local state: %+v", r)
	}
	calls := o.counts()
	if calls != 1 {
		t.Fatalf("same-directory reset made %d CDN calls", calls)
	}
	reset.stop()
	c.StateDir = t.TempDir()
	fresh := start(t, c)
	requireVersion(t, read(fresh.url, "prod", 0), "v1")
	calls = o.counts()
	if calls != 2 {
		t.Fatalf("fresh environment reused another environment's disk: %d", calls)
	}
	t.Log("same-directory process reset reused disk; fresh environment directory required another fetch")
}

func TestMaximumAgeStopsUnboundedStaleServing(t *testing.T) {
	o := newOrigin(t)
	c := defaults(o)
	c.TTL, c.MaxAge = 0, 100*time.Millisecond
	p := start(t, c)
	requireVersion(t, read(p.url, "prod", 0), "v1")
	o.set(func() { o.status = 503 })
	time.Sleep(160 * time.Millisecond)
	if r := read(p.url, "prod", 0); r.status != 503 {
		t.Fatalf("served beyond maximum age: %+v", r)
	}
}

func Test304RequiresAcceptedConfiguration(t *testing.T) {
	o := newOrigin(t)
	o.status = 304
	p := start(t, defaults(o))
	if r := read(p.url, "prod", 0); r.status != 503 {
		t.Fatalf("304 bootstrapped without configuration: %+v", r)
	}
}

func mustJSON(value any) []byte { b, _ := json.Marshal(value); return b }

func fixture(version int, environment string) json.RawMessage {
	value := version >= 2
	config := map[string]any{"data": map[string]any{"type": "universal-flag-configuration", "attributes": map[string]any{
		"createdAt": fmt.Sprintf("2026-11-17T00:00:%02d.000Z", version), "format": "SERVER", "environment": map[string]any{"name": environment},
		"flags": map[string]any{"kill-switch": map[string]any{
			"key": "kill-switch", "enabled": true, "variationType": "BOOLEAN",
			"variations":  map[string]any{"selected": map[string]any{"key": "selected", "value": value}},
			"allocations": []any{map[string]any{"key": "experiment-allocation", "rules": []any{}, "splits": []any{map[string]any{"variationKey": "selected", "shards": []any{}}}, "doLog": true}},
		}},
	}}}
	b, _ := json.Marshal(config)
	return b
}
