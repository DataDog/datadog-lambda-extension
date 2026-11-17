package main

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"time"
)

// Document records the raw UFC response and the last successful CDN validation.
type Document struct {
	Scope         string          `json:"scope"`
	Version       string          `json:"version"`
	ValidatedAt   time.Time       `json:"validated_at"`
	Configuration json.RawMessage `json:"configuration"`
}

type Config struct {
	Mode           string        `json:"mode"` // passthrough or cache
	Upstream       string        `json:"upstream"`
	TTL            time.Duration `json:"ttl"`
	MaxAge         time.Duration `json:"max_age"`
	Timeout        time.Duration `json:"timeout"`
	FailureBackoff time.Duration `json:"failure_backoff"`
	StateDir       string        `json:"state_dir"`
}

type entry struct {
	Document  Document  `json:"document"`
	ETag      string    `json:"etag"`
	CheckedAt time.Time `json:"checked_at"`
}

type bucket struct {
	gate     chan struct{}
	value    *entry
	loaded   bool
	retryAt  time.Time
	fromDisk bool
}

type Proxy struct {
	config  Config
	client  *http.Client
	now     func() time.Time
	mu      sync.Mutex
	buckets map[string]*bucket
}

func NewProxy(c Config) *Proxy {
	return &Proxy{config: c, client: &http.Client{}, now: time.Now, buckets: make(map[string]*bucket)}
}

func requestScope(r *http.Request) string {
	q := url.Values{}
	q.Set("query", r.URL.Query().Encode())
	q.Set("accept", r.Header.Get("Accept"))
	return q.Encode()
}

func (p *Proxy) getBucket(scope string) *bucket {
	p.mu.Lock()
	defer p.mu.Unlock()
	b := p.buckets[scope]
	if b == nil {
		b = &bucket{gate: make(chan struct{}, 1)}
		p.buckets[scope] = b
	}
	return b
}

func (p *Proxy) usable(e *entry, scope string, now time.Time) bool {
	if e == nil || e.Document.Scope != scope || e.Document.Version == "" || !json.Valid(e.Document.Configuration) {
		return false
	}
	age := now.Sub(e.Document.ValidatedAt)
	return !e.Document.ValidatedAt.IsZero() && age >= -time.Second && age <= p.config.MaxAge
}

func (p *Proxy) statePath(scope string) string {
	return filepath.Join(p.config.StateDir, fmt.Sprintf("%x.json", sha256.Sum256([]byte(p.config.Upstream+"\n"+scope))))
}

func (p *Proxy) load(scope string) *entry {
	if p.config.StateDir == "" {
		return nil
	}
	data, err := os.ReadFile(p.statePath(scope))
	if err != nil {
		return nil
	}
	var e entry
	if json.Unmarshal(data, &e) != nil || !p.usable(&e, scope, p.now()) {
		return nil
	}
	return &e
}

func (p *Proxy) persist(scope string, e *entry) {
	if p.config.StateDir == "" {
		return
	}
	if os.MkdirAll(p.config.StateDir, 0700) != nil {
		return
	}
	data, _ := json.Marshal(e)
	f, err := os.CreateTemp(p.config.StateDir, ".snapshot-")
	if err != nil {
		return
	}
	name := f.Name()
	defer os.Remove(name)
	_, err = f.Write(data)
	if closeErr := f.Close(); err == nil {
		err = closeErr
	}
	if err == nil {
		_ = os.Rename(name, p.statePath(scope))
	}
}

func (p *Proxy) fetch(ctx context.Context, r *http.Request, previous *entry) (*entry, error) {
	u, err := url.Parse(p.config.Upstream)
	if err != nil {
		return nil, err
	}
	u.RawQuery = r.URL.Query().Encode()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Accept", r.Header.Get("Accept"))
	if previous != nil {
		req.Header.Set("If-None-Match", previous.ETag)
	}
	resp, err := p.client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode == http.StatusNotModified {
		if previous == nil {
			return nil, errors.New("304 without accepted configuration")
		}
		e := *previous
		e.CheckedAt = p.now()
		e.Document.ValidatedAt = p.now()
		if !p.usable(&e, requestScope(r), p.now()) {
			return nil, errors.New("configuration exceeds maximum accepted age")
		}
		return &e, nil
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("upstream status %d", resp.StatusCode)
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, (8<<20)+1))
	if err != nil || len(data) > 8<<20 {
		return nil, errors.New("unreadable or oversized response")
	}
	var response struct {
		Data struct {
			Attributes struct {
				Format string                     `json:"format"`
				Flags  map[string]json.RawMessage `json:"flags"`
			} `json:"attributes"`
		} `json:"data"`
	}
	if json.Unmarshal(data, &response) != nil || response.Data.Attributes.Format != "SERVER" || response.Data.Attributes.Flags == nil {
		return nil, errors.New("invalid server UFC response")
	}
	d := Document{Scope: requestScope(r), Version: resp.Header.Get("ETag"), ValidatedAt: p.now(), Configuration: data}
	e := &entry{Document: d, ETag: resp.Header.Get("ETag"), CheckedAt: p.now()}
	if !p.usable(e, requestScope(r), p.now()) {
		return nil, errors.New("snapshot is invalid, expired, or scoped differently")
	}
	return e, nil
}

func (p *Proxy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path != "/api/v2/feature-flagging/config/rules-based/server" {
		http.NotFound(w, r)
		return
	}
	if r.Method != http.MethodGet {
		w.Header().Set("Allow", http.MethodGet)
		http.Error(w, "use GET", http.StatusMethodNotAllowed)
		return
	}
	budget := p.config.Timeout
	if ms, err := strconv.Atoi(r.Header.Get("X-Config-Budget-Ms")); err == nil && ms > 0 && time.Duration(ms)*time.Millisecond < budget {
		budget = time.Duration(ms) * time.Millisecond
	}
	ctx, cancel := context.WithTimeout(r.Context(), budget)
	defer cancel()
	scope := requestScope(r)
	if p.config.Mode == "passthrough" {
		e, err := p.fetch(ctx, r, nil)
		if err != nil {
			http.Error(w, err.Error(), http.StatusServiceUnavailable)
			return
		}
		p.reply(w, e, "passthrough")
		return
	}
	b := p.getBucket(scope)
	select {
	case b.gate <- struct{}{}:
		defer func() { <-b.gate }()
	case <-ctx.Done():
		http.Error(w, "configuration wait budget exhausted", http.StatusServiceUnavailable)
		return
	}
	if !b.loaded {
		b.value = p.load(scope)
		b.fromDisk = b.value != nil
		b.loaded = true
	}
	now := p.now()
	if p.usable(b.value, scope, now) && now.Sub(b.value.CheckedAt) < p.config.TTL {
		source := "memory"
		if b.fromDisk {
			source = "disk"
		}
		p.reply(w, b.value, source)
		return
	}
	var fetchErr error
	if !now.Before(b.retryAt) {
		var e *entry
		e, fetchErr = p.fetch(ctx, r, b.value)
		if fetchErr == nil {
			b.value, b.fromDisk = e, false
			b.retryAt = time.Time{}
			p.persist(scope, e)
			p.reply(w, e, "refresh")
			return
		}
		b.retryAt = p.now().Add(p.config.FailureBackoff)
	}
	if p.usable(b.value, scope, p.now()) {
		p.reply(w, b.value, "stale")
		return
	}
	http.Error(w, fmt.Sprintf("no usable configuration: %v", fetchErr), http.StatusServiceUnavailable)
}

func (p *Proxy) reply(w http.ResponseWriter, e *entry, source string) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("X-Config-Version", e.Document.Version)
	w.Header().Set("X-Config-Source", source)
	w.Header().Set("X-Origin-Age-Ms", strconv.FormatInt(p.now().Sub(e.Document.ValidatedAt).Milliseconds(), 10))
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(e.Document.Configuration)
}

func serve(c Config, addr string) error {
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return err
	}
	_ = json.NewEncoder(os.Stdout).Encode(map[string]string{"url": "http://" + ln.Addr().String()})
	server := &http.Server{Handler: NewProxy(c), ReadHeaderTimeout: 5 * time.Second}
	var lifecycleResult chan error
	if runtimeAPI := os.Getenv("AWS_LAMBDA_RUNTIME_API"); runtimeAPI != "" {
		lifecycleResult = make(chan error, 1)
		go func() {
			lifecycleResult <- lambdaLifecycle(context.Background(), runtimeAPI)
			_ = server.Close()
		}()
	}
	err = server.Serve(ln)
	if errors.Is(err, http.ErrServerClosed) {
		return <-lifecycleResult
	}
	return err
}

func main() {
	c := Config{}
	flag.StringVar(&c.Mode, "mode", "cache", "passthrough or cache")
	flag.StringVar(&c.Upstream, "upstream", "", "configuration endpoint; local fake CDN for the example")
	flag.DurationVar(&c.TTL, "ttl", 30*time.Second, "minimum interval between upstream checks")
	flag.DurationVar(&c.MaxAge, "max-age", 5*time.Minute, "maximum accepted origin validation age")
	flag.DurationVar(&c.Timeout, "timeout", 100*time.Millisecond, "upstream and lock wait budget")
	flag.DurationVar(&c.FailureBackoff, "failure-backoff", time.Second, "delay between failed refreshes")
	flag.StringVar(&c.StateDir, "state-dir", "", "optional environment-local persistence directory")
	addr := flag.String("listen", "127.0.0.1:0", "listen address")
	flag.Parse()
	if c.Upstream == "" || (c.Mode != "passthrough" && c.Mode != "cache") || c.Timeout <= 0 || c.TTL < 0 || c.MaxAge <= 0 || c.MaxAge < c.TTL || c.FailureBackoff < 0 {
		fmt.Fprintln(os.Stderr, "supply an upstream; require a valid mode, positive timeout and max-age, max-age >= ttl >= 0, and nonnegative failure-backoff")
		os.Exit(2)
	}
	if err := serve(c, *addr); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
