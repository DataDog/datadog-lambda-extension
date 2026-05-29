# OOM reproducer for Python.
# Allocates and retains 10 MB strings in a list until CPython raises
# MemoryError. Lambda surfaces this as PlatformRuntimeDone with
# error_type=Runtime.OutOfMemory; the function log line also contains
# "MemoryError". Both bottlecap detection paths fire — the dedup flag is
# what makes the OOM metric emit exactly once.


def handler(event, context):
    data = []
    while True:
        data.append("x" * (10 * 1024 * 1024))
