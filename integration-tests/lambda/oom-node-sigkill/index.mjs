// OOM reproducer: off-heap Buffer growth → kernel SIGKILL.
// Buffer.allocUnsafe(>8KB) goes through V8's ArrayBuffer allocator (external
// memory) and bypasses --max-old-space-size, so RSS grows until the cgroup
// limit triggers a kernel SIGKILL. Lambda surfaces this as PlatformRuntimeDone
// with error_type=Runtime.OutOfMemory — bottlecap's path 2 detection.
export const handler = async () => {
  const bufs = [];
  while (true) {
    const b = Buffer.allocUnsafe(20 * 1024 * 1024);
    b.fill(0);
    bufs.push(b);
  }
};
