package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
)

// The optional lifecycle client makes the example usable as an external Lambda extension.
// The cache remains live while the runtime handles an invocation, including after /next.
func lambdaLifecycle(ctx context.Context, runtimeAPI string) error {
	base := "http://" + runtimeAPI + "/2020-01-01/extension"
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, base+"/register", bytes.NewBufferString(`{"events":["INVOKE","SHUTDOWN"]}`))
	if err != nil {
		return err
	}
	request.Header.Set("Lambda-Extension-Name", filepath.Base(os.Args[0]))
	request.Header.Set("Content-Type", "application/json")
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		return err
	}
	identifier := response.Header.Get("Lambda-Extension-Identifier")
	_, _ = io.Copy(io.Discard, response.Body)
	_ = response.Body.Close()
	if response.StatusCode != 200 || identifier == "" {
		return fmt.Errorf("extension registration failed: %d", response.StatusCode)
	}
	for {
		request, err = http.NewRequestWithContext(ctx, http.MethodGet, base+"/event/next", nil)
		if err != nil {
			return err
		}
		request.Header.Set("Lambda-Extension-Identifier", identifier)
		response, err = http.DefaultClient.Do(request)
		if err != nil {
			return err
		}
		var event struct {
			EventType string `json:"eventType"`
		}
		err = json.NewDecoder(response.Body).Decode(&event)
		_ = response.Body.Close()
		if err != nil || response.StatusCode != 200 {
			return fmt.Errorf("extension next event failed: status=%d error=%v", response.StatusCode, err)
		}
		if event.EventType == "SHUTDOWN" {
			return nil
		}
	}
}
