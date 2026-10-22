// Shared scenario config for the payload-size suites. 400 x 24 KB enriches to
// a ~10 MB trace: large enough to exercise the extension's high batch cap,
// yet under the 12 MB cap so it flushes in a single batch without a 413.
export const SPAN_COUNT = 400;
export const PAYLOAD_BYTES = 24_000;

// A cold invocation delivers its large (~10 MB) trace to the extension too
// late to make that invocation's end-of-invocation flush, so it flushes on a
// following invocation. The extra invocations give the first request's trace
// a flush to ride out on.
export const INVOCATION_COUNT = 3;
export const DELAY_BETWEEN_INVOCATIONS_MS = 2000;

export function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}
