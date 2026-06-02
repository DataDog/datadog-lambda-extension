# OOM reproducer for Python on LMI (memory >= 2048 MB).
# A single 100 GB bytearray allocation request exceeds any reasonable
# Lambda memory cap by orders of magnitude, so Python's C allocator
# refuses immediately and raises MemoryError before the kernel has a
# chance to involve the cgroup OOM killer with a SIGKILL. This is the
# fastest and most reliable way to surface a clean MemoryError log
# line on a 2 GB function. The 10 MB-loop pattern used by `oom-python`
# would either take too long or get SIGKILL'd silently at 2 GB.
def handler(event, context):
    data = bytearray(100 * 1024 * 1024 * 1024)
