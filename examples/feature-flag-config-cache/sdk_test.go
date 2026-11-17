//go:build darwin || linux

package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
)

type sdkResult struct {
	Value                bool              `json:"value"`
	Variant              string            `json:"variant"`
	Reason               string            `json:"reason"`
	Error                string            `json:"error"`
	ErrorCode            string            `json:"errorCode"`
	ConfigurationVersion string            `json:"configurationVersion"`
	CacheSource          string            `json:"cacheSource"`
	SDK                  map[string]string `json:"sdk"`
	Exposures            []map[string]any  `json:"exposures"`
}

type sdkApp struct {
	cmd   *exec.Cmd
	input io.WriteCloser
	lines chan string
	once  sync.Once
}

func startSDK(t *testing.T, cacheURL string) *sdkApp {
	t.Helper()
	if _, err := os.Stat("node_modules/@datadog/openfeature-node-server/package.json"); err != nil {
		t.Fatal("install the pinned real SDK first: npm ci --ignore-scripts")
	}
	cmd := exec.Command("node", "app.mjs")
	cmd.Env = append(os.Environ(), "CONFIG_CACHE_URL="+cacheURL, "DD_ENV=prod")
	cmd.Stderr = os.Stderr
	input, err := cmd.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	output, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err = cmd.Start(); err != nil {
		t.Fatal(err)
	}
	app := &sdkApp{cmd: cmd, input: input, lines: make(chan string, 1)}
	go func() {
		scanner := bufio.NewScanner(output)
		for scanner.Scan() {
			app.lines <- scanner.Text()
		}
		close(app.lines)
	}()
	t.Cleanup(func() { app.stop(t) })
	return app
}

func (s *sdkApp) stop(t *testing.T) {
	t.Helper()
	s.once.Do(func() {
		_ = s.cmd.Process.Signal(syscall.SIGCONT)
		_ = s.input.Close()
		done := make(chan error, 1)
		go func() { done <- s.cmd.Wait() }()
		select {
		case err := <-done:
			if err != nil {
				t.Errorf("SDK application shutdown: %v", err)
			}
		case <-time.After(3 * time.Second):
			_ = s.cmd.Process.Kill()
			<-done
			t.Error("SDK application did not shut down")
		}
	})
}

func (s *sdkApp) invoke(t *testing.T, subject string) sdkResult {
	t.Helper()
	_, err := fmt.Fprintf(s.input, "{\"targetingKey\":%q}\n", subject)
	if err != nil {
		t.Fatal(err)
	}
	select {
	case line, ok := <-s.lines:
		if !ok {
			t.Fatal("real SDK example exited without a result")
		}
		var r sdkResult
		if err = json.Unmarshal([]byte(line), &r); err != nil || r.Error != "" {
			t.Fatalf("SDK application failed: %s; %v", line, err)
		}
		if r.SDK["name"] != "@datadog/openfeature-node-server" || r.SDK["version"] != "2.7.0" || !strings.Contains(r.SDK["resolved"], "node_modules/@datadog/openfeature-node-server/") {
			t.Fatalf("unexpected loaded SDK artifact: %+v", r.SDK)
		}
		return r
	case <-time.After(5 * time.Second):
		t.Fatal("real SDK invocation exceeded test deadline")
	}
	return sdkResult{}
}

func requireEvaluation(t *testing.T, r sdkResult, value bool) {
	t.Helper()
	if r.Value != value || r.Variant != "selected" || r.ErrorCode != "" || r.Reason == "DEFAULT" || r.Reason == "ERROR" {
		t.Fatalf("expected real flag evaluation value=%v, got %+v", value, r)
	}
}

func TestRealSDKCachedInvocationsReduceCDNRequests(t *testing.T) {
	for _, mode := range []string{"passthrough", "cache"} {
		t.Run(mode, func(t *testing.T) {
			o := newOrigin(t)
			o.version = 2 // The configured true value differs from the application's false default.
			c := defaults(o)
			c.Mode = mode
			proxy := start(t, c)
			app := startSDK(t, proxy.url)
			for i := 0; i < 100; i++ {
				r := app.invoke(t, "same-subject")
				requireEvaluation(t, r, true)
				wantExposures := 0
				if i == 0 {
					wantExposures = 1
				}
				if len(r.Exposures) != wantExposures {
					t.Fatalf("exposure deduplication changed at invocation %d: %+v", i, r.Exposures)
				}
			}
			calls := o.counts()
			want := 1
			if mode == "passthrough" {
				want = 100
			}
			if calls != want {
				t.Fatalf("100 real SDK invocations: want %d CDN requests, got %d", want, calls)
			}
			t.Logf("100 invocations; real Datadog SDK 2.7.0; SDK poller absent; mode=%s; CDN=%d; true evaluations=100; exposures=1", mode, calls)
		})
	}
}

func TestRealSDKRefreshesConfigurationAndRetainsExposureReporting(t *testing.T) {
	o := newOrigin(t)
	c := defaults(o)
	c.TTL = 0
	proxy := start(t, c)
	app := startSDK(t, proxy.url)
	first := app.invoke(t, "subject-a")
	requireEvaluation(t, first, false)
	if len(first.Exposures) != 1 {
		t.Fatalf("first assignment did not emit its exposure: %+v", first)
	}
	o.set(func() { o.version = 2 })
	second := app.invoke(t, "subject-a")
	requireEvaluation(t, second, true)
	if second.ConfigurationVersion != "v2" || len(second.Exposures) != 1 {
		t.Fatalf("new configuration did not update evaluation and exposure state: %+v", second)
	}
	third := app.invoke(t, "subject-b")
	requireEvaluation(t, third, true)
	if len(third.Exposures) != 1 {
		t.Fatal("different subject lost its exposure")
	}
	t.Log("real provider: false/v1 -> true/v2; new configuration and new subjects preserve exposures")
}

func TestRealSDKExpiresRulesWhenExtensionCannotSupplyUsableConfiguration(t *testing.T) {
	o := newOrigin(t)
	o.version = 2
	c := defaults(o)
	c.TTL, c.MaxAge = 0, 100*time.Millisecond
	proxy := start(t, c)
	app := startSDK(t, proxy.url)
	requireEvaluation(t, app.invoke(t, "subject-a"), true)
	o.set(func() { o.status = 503 })
	<-time.After(150 * time.Millisecond)
	r := app.invoke(t, "subject-a")
	if r.Value || r.ErrorCode == "" || len(r.Exposures) != 0 {
		t.Fatalf("expired SDK rules remained usable: %+v", r)
	}
	t.Logf("expired extension snapshot withdrawn from real SDK: default=false, error=%s, exposures=0", r.ErrorCode)
	o.set(func() { o.status = 0 })
	// A later request must be allowed to retry and recover the same SDK instance.
	time.Sleep(c.FailureBackoff)
	requireEvaluation(t, app.invoke(t, "subject-a"), true)
}

func TestRealSDKRuntimeRestartsReuseExtensionCache(t *testing.T) {
	o := newOrigin(t)
	o.version = 2
	proxy := start(t, defaults(o))
	for i := 0; i < 5; i++ {
		app := startSDK(t, proxy.url)
		r := app.invoke(t, "same-subject")
		requireEvaluation(t, r, true)
		if len(r.Exposures) != 1 {
			t.Fatal("a new SDK instance did not emit its exposure")
		}
		app.stop(t)
	}
	if calls := o.counts(); calls != 1 {
		t.Fatalf("five fresh SDK instances made %d CDN requests", calls)
	}
	t.Log("five fresh Node SDK processes bootstrapped from one extension fetch; SDK exposure state remains per process")
}

func TestRealSDKRefreshesAfterIdleAndFreezeThaw(t *testing.T) {
	o := newOrigin(t)
	c := defaults(o)
	c.TTL = 100 * time.Millisecond
	proxy := start(t, c)
	app := startSDK(t, proxy.url)
	requireEvaluation(t, app.invoke(t, "subject-a"), false)
	time.Sleep(180 * time.Millisecond)
	if calls := o.counts(); calls != 1 {
		t.Fatalf("an idle SDK or extension polled the CDN: %d", calls)
	}
	pause(t, app.cmd.Process)
	pause(t, proxy.cmd.Process)
	o.set(func() { o.version = 2 })
	time.Sleep(180 * time.Millisecond)
	if calls := o.counts(); calls != 1 {
		t.Fatalf("work occurred while both processes were paused: %d", calls)
	}
	for _, process := range []*os.Process{proxy.cmd.Process, app.cmd.Process} {
		if err := process.Signal(syscall.SIGCONT); err != nil {
			t.Fatal(err)
		}
	}
	r := app.invoke(t, "subject-a")
	requireEvaluation(t, r, true)
	if r.ConfigurationVersion != "v2" || o.counts() != 2 || len(r.Exposures) != 1 {
		t.Fatalf("first resumed invocation did not refresh once: %+v; CDN calls=%d", r, o.counts())
	}
	t.Log("no idle polling; paused SDK and extension resumed; first invocation refreshed and evaluated v2")
}
