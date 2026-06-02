// OOM reproducer for Go.
// Allocates a 500 MB byte slice in a single shot, then writes to every page
// to force physical commit. On a 256 MB Lambda (see `oomMemorySize` in
// `lib/stacks/oom.ts`) this immediately exceeds the cgroup memory limit and
// the kernel SIGKILLs the process, producing a PlatformReport with
// max_memory_used_mb == memory_size_mb. The Go runtime
// also typically prints "fatal error: runtime: out of memory" on the way
// down — bottlecap's runtime-specific log-line detection matches that
// message. Per-Context dedup ensures the OOM metric increments only once
// even if both paths fire.
package main

import (
	"log"

	"github.com/aws/aws-lambda-go/lambda"
)

func handler() error {
	log.Println("OOM reproducer: allocating 500 MB")
	b := make([]byte, 500*1024*1024)
	for i := range b {
		b[i] = byte(i % 256)
	}
	log.Println("did not OOM — unexpected") // unreachable on a 256 MB Lambda
	return nil
}

func main() {
	lambda.Start(handler)
}
