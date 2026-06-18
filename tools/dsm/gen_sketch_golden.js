#!/usr/bin/env node
/*
 * Golden-vector generator for the DSM DDSketch encoding.
 *
 * Reuses the *exact* sketch implementation the dd-trace-js tracer ships
 * (`LogCollapsingLowestDenseDDSketch`, relativeAccuracy 0.01, binLimit 2048)
 * and dumps, for a set of fixed inputs:
 *   - the serialized DDSketch protobuf bytes (hex) -- this is the blob the
 *     tracer embeds as EdgeLatency / PathwayLatency / PayloadSize.
 *   - the decoded structure (gamma, indexOffset, interpolation,
 *     contiguousBinIndexOffset, contiguousBinCounts, zeroCount) for sanity.
 *
 * Output is JSON on stdout. These vectors are pinned in the Rust unit tests so
 * our hand-rolled sketch must reproduce them byte-for-byte.
 *
 * Read-only: requires the vendored sketches-js bundle by absolute path and
 * writes nothing.
 */

'use strict'

// Absolute path to the dd-trace-js vendored sketches-js bundle.
const SKETCHES_JS = '/Users/james.eastham/source/datadog/dd-trace-js/vendor/dist/@datadog/sketches-js'
// The compiled proto, used only to DECODE our own output for the readable dump.
const PROTO = '/Users/james.eastham/source/datadog/dd-trace-js/vendor/node_modules/@datadog/sketches-js/dist/ddsketch/proto/compiled.js'

const { LogCollapsingLowestDenseDDSketch } = require(SKETCHES_JS)
const { DDSketch: DDSketchProto } = require(PROTO)

// Fixed input cases. Latency values are in SECONDS (the tracer feeds ns / 1e9);
// the payload-size case uses raw byte counts. Names are stable test IDs.
const CASES = [
  { name: 'single_value_1s', values: [1.0] },
  { name: 'single_value_tenth', values: [0.1] },
  { name: 'single_value_1ms', values: [0.001] },
  { name: 'multi_spread', values: [0.001, 0.01, 0.1, 1.0, 10.0] },
  { name: 'repeated_same', values: [0.5, 0.5, 0.5, 0.5] },
  { name: 'payload_sizes', values: [100, 256, 1024, 4096] },
  { name: 'zero_value', values: [0.0] },
]

function dump(values) {
  const sketch = new LogCollapsingLowestDenseDDSketch()
  for (const v of values) sketch.accept(v)

  const bytes = Buffer.from(sketch.toProto())
  const decoded = DDSketchProto.toObject(DDSketchProto.decode(bytes), {
    longs: String,
    enums: String,
    defaults: true,
  })

  return {
    valueHex: bytes.toString('hex'),
    valueBase64: bytes.toString('base64'),
    byteLen: bytes.length,
    decoded,
  }
}

const out = {
  generator: 'dd-trace-js vendored @datadog/sketches-js',
  sketch: 'LogCollapsingLowestDenseDDSketch (relativeAccuracy=0.01, binLimit=2048)',
  cases: CASES.map((c) => ({ name: c.name, values: c.values, ...dump(c.values) })),
}

process.stdout.write(JSON.stringify(out, null, 2) + '\n')
