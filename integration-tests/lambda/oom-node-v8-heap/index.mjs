// OOM reproducer: classic V8 heap exhaustion. Allocates retained strings in a
// loop until V8 hits its --max-old-space-size cap and prints
// "FATAL ERROR: ... JavaScript heap out of memory". Exercises bottlecap's
// runtime-specific log-line OOM detection path.
export const handler = async () => {
  const arr = [];
  while (true) {
    arr.push('x'.repeat(10 * 1024 * 1024));
  }
};
