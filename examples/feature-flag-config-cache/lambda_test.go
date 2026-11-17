//go:build darwin || linux

package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestLambdaLifecyclePreservesCacheAcrossInvocationsAndShutsDown(t *testing.T) {
	events := make(chan string)
	nextRequests := make(chan struct{}, 4)
	api := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/2020-01-01/extension/register":
			var body struct {
				Events []string `json:"events"`
			}
			if r.Method != "POST" || r.Header.Get("Lambda-Extension-Name") != filepath.Base(os.Args[0]) || json.NewDecoder(r.Body).Decode(&body) != nil || strings.Join(body.Events, ",") != "INVOKE,SHUTDOWN" {
				t.Error("invalid extension registration")
				w.WriteHeader(400)
				return
			}
			w.Header().Set("Lambda-Extension-Identifier", "test-extension-id")
			_, _ = fmt.Fprint(w, `{}`)
		case "/2020-01-01/extension/event/next":
			if r.Method != "GET" || r.Header.Get("Lambda-Extension-Identifier") != "test-extension-id" {
				t.Error("missing extension identifier")
				w.WriteHeader(400)
				return
			}
			nextRequests <- struct{}{}
			select {
			case event := <-events:
				_ = json.NewEncoder(w).Encode(map[string]string{"eventType": event})
			case <-r.Context().Done():
			}
		default:
			http.NotFound(w, r)
		}
	}))
	t.Cleanup(api.Close)
	o := newOrigin(t)
	o.version = 2
	proxy := start(t, defaults(o), "AWS_LAMBDA_RUNTIME_API="+strings.TrimPrefix(api.URL, "http://"))
	awaitNext := func() {
		t.Helper()
		select {
		case <-nextRequests:
		case <-time.After(3 * time.Second):
			t.Fatal("extension did not request its next lifecycle event")
		}
	}
	send := func(event string) {
		t.Helper()
		select {
		case events <- event:
		case <-time.After(3 * time.Second):
			t.Fatal("extension did not consume lifecycle event")
		}
	}
	awaitNext() // Extension init is complete. The HTTP server must remain available.
	app := startSDK(t, proxy.url)
	for i := 0; i < 2; i++ {
		send("INVOKE")
		awaitNext() // Extension completes its event work while the runtime still runs.
		requireEvaluation(t, app.invoke(t, "subject-a"), true)
	}
	if calls := o.counts(); calls != 1 {
		t.Fatalf("lifecycle events discarded the warm cache: %d CDN calls", calls)
	}
	app.stop(t)
	send("SHUTDOWN")
	select {
	case err := <-proxy.exit:
		if err != nil {
			t.Fatalf("extension shutdown: %v", err)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("extension did not exit after SHUTDOWN")
	}
	if r := read(proxy.url, "prod", 0); r.err == nil {
		t.Fatal("cache listener remained available after SHUTDOWN")
	}
	t.Log("registered; two INVOKE events retained one cache; SDK evaluated after /next; SHUTDOWN closed the process")
}

func TestLambdaRegistrationFailureExitsProcess(t *testing.T) {
	api := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(500) }))
	t.Cleanup(api.Close)
	o := newOrigin(t)
	proxy := start(t, defaults(o), "AWS_LAMBDA_RUNTIME_API="+strings.TrimPrefix(api.URL, "http://"))
	select {
	case err := <-proxy.exit:
		if err == nil {
			t.Fatal("registration failure returned success")
		}
	case <-time.After(3 * time.Second):
		t.Fatal("registration failure did not stop the process")
	}
}
