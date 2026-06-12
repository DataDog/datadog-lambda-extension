# OOM reproducer for Ruby.
# Allocates and retains 10 MB strings in an array until Ruby raises
# NoMemoryError. Lambda surfaces this as PlatformRuntimeDone with
# error_type=Runtime.OutOfMemory; the function log line also contains
# "failed to allocate memory (NoMemoryError)". Both bottlecap detection
# paths fire — the dedup flag is what makes the OOM metric emit exactly once.

def handler(event:, context:)
  data = []
  loop do
    data << ("x" * (10 * 1024 * 1024))
  end
end
