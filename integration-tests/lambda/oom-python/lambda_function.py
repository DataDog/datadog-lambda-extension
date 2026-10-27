# OOM reproducer for Python.
# A single 100 GB bytearray request exceeds the function's 512 MB memory
# cap by orders of magnitude. The handler is expected to raise an uncaught
# MemoryError, which Lambda surfaces as a "MemoryError" log line and a
# PlatformRuntimeDone with error_type=Runtime.OutOfMemory; bottlecap
# deduplication keeps the OOM metric at exactly one emission. Linux
# overcommit means this is not guaranteed to fail deterministically at
# the allocator: the kernel may terminate the process instead.
def handler(event, context):
    data = bytearray(100 * 1024 * 1024 * 1024)
