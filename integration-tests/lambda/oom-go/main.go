// OOM reproducer for Go.
// Allocates and retains 10 MB byte slices in a slice header until the Go
// runtime aborts with "fatal error: runtime: out of memory". Bottlecap's
// runtime-specific log-line detection matches that fatal-error message.
// Without that detection (and historically for provided.al runtimes), the
// equality path in PlatformReport (max_memory_used_mb == memory_size_mb) also
// fires. The per-Context dedup flag ensures the metric increments only once.
package main

import (
	"github.com/aws/aws-lambda-go/lambda"
)

func handler() error {
	var data [][]byte
	for {
		data = append(data, make([]byte, 10*1024*1024))
	}
}

func main() {
	lambda.Start(handler)
}
