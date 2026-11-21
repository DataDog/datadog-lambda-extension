'use strict'

// Process-level tests: the real Extension binary (test-mode build) caches the
// configuration from a fake CDN, and the real dd-trace flag provider
// evaluates it in a separate process per simulated execution environment.
// Nothing here runs in AWS Lambda.

const assert = require('node:assert/strict')
const { fork, spawn } = require('node:child_process')
const fs = require('node:fs')
const http = require('node:http')
const os = require('node:os')
const path = require('node:path')
const { setTimeout: sleep } = require('node:timers/promises')
const { afterEach, test } = require('node:test')

const { FLAG_CONFIG_PATH } = require('../extension-config')

const BOTTLECAP_BIN = process.env.BOTTLECAP_BIN ??
  path.join(__dirname, '../../../bottlecap/target/debug/bottlecap-test-mode')
const FIXTURES = {
  v1: fs.readFileSync(path.join(__dirname, '../fixtures/ufc-v1.json'), 'utf8'),
  v2: fs.readFileSync(path.join(__dirname, '../fixtures/ufc-v2.json'), 'utf8'),
  invalid: '<html>502 Bad Gateway</html>',
}
const US_USER = { userId: 'user-1', country: 'US' }
const FR_USER = { userId: 'user-2', country: 'France' }

const cleanups = []
afterEach(async () => {
  while (cleanups.length) await cleanups.pop()()
})

function listen (handler) {
  const server = http.createServer(handler)
  return new Promise(resolve => server.listen(0, '127.0.0.1', () => {
    cleanups.push(() => new Promise(r => { server.closeAllConnections(); server.close(r) }))
    resolve({ server, url: `http://127.0.0.1:${server.address().port}` })
  }))
}

async function freePort () {
  const server = http.createServer()
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  const { port } = server.address()
  await new Promise(resolve => server.close(resolve))
  return port
}

/** Stands in for the Datadog CDN and records every request it gets. */
async function startCdn () {
  const cdn = { mode: 'v1', delayMs: 0, requests: [] }
  const { url } = await listen(async (req, res) => {
    const { pathname, searchParams } = new URL(req.url, 'http://x')
    cdn.requests.push({
      path: pathname,
      apiKey: req.headers['dd-api-key'],
      env: searchParams.get('dd_env'),
      ifNoneMatch: req.headers['if-none-match'],
    })
    if (cdn.delayMs) await sleep(cdn.delayMs)
    if (cdn.mode === 'down') return res.writeHead(503).end()
    const etag = `"${cdn.mode}"`
    if (req.headers['if-none-match'] === etag) return res.writeHead(304, { ETag: etag }).end()
    res.writeHead(200, { 'Content-Type': 'application/json', ETag: etag }).end(FIXTURES[cdn.mode])
  })
  cdn.url = url
  return cdn
}

/** Stands in for the trace agent endpoints the tracer reports to. */
async function startAgent () {
  const agent = { requests: [] }
  const { url } = await listen((req, res) => {
    let body = ''
    req.on('data', chunk => { body += chunk })
    req.on('end', () => {
      agent.requests.push({ method: req.method, path: req.url, body })
      if (req.url.startsWith('/info')) {
        return res.writeHead(200, { 'Content-Type': 'application/json' })
          .end(JSON.stringify({ endpoints: ['/v0.4/traces', '/evp_proxy/v2/'] }))
      }
      res.writeHead(200, { 'Content-Type': 'application/json' }).end('{}')
    })
  })
  agent.url = url
  return agent
}

/** Counts any request: the SDK's own upstream poller would land here. */
async function startTrap () {
  const trap = { hits: 0 }
  const { url } = await listen((req, res) => { trap.hits++; res.writeHead(500).end() })
  trap.url = url
  return trap
}

/** The real Extension, built with `cargo build --features test-mode`. */
async function startExtension (cdn, env = {}) {
  assert.ok(fs.existsSync(BOTTLECAP_BIN), `build ${BOTTLECAP_BIN} first (see README)`)
  const port = await freePort()
  const child = spawn(BOTTLECAP_BIN, [], {
    env: {
      PATH: process.env.PATH,
      DD_APM_RECEIVER_PORT: String(port),
      DD_TESTMODE_FEATURE_FLAGS_CDN_URL: cdn.url,
      DD_APM_DD_URL: 'http://127.0.0.1:9',
      DD_ENV: 'prod',
      DD_LOG_LEVEL: 'error',
      // The fake CDN speaks plain HTTP/1.1.
      DD_HTTP_PROTOCOL: 'http1',
      DD_FEATURE_FLAGS_EXTENSION_CACHE_ENABLED: 'true',
      ...env,
    },
    stdio: ['ignore', 'ignore', 'pipe'],
  })
  let stderr = ''
  child.stderr.on('data', chunk => { stderr += chunk })
  cleanups.push(() => new Promise(resolve => {
    if (child.exitCode !== null || child.signalCode !== null) return resolve()
    child.once('exit', resolve)
    child.kill('SIGKILL')
  }))
  const url = `http://127.0.0.1:${port}`
  for (let i = 0; i < 100; i++) {
    const status = await fetch(`${url}/info`).then(r => r.status, () => 0)
    if (status === 200) return { url, child, stderr: () => stderr }
    await sleep(50)
  }
  throw new Error(`extension did not start: ${stderr}`)
}

/** A fresh Node process with the real tracer: one execution environment. */
async function startEnvironment (extension, agent, trap, env = {}) {
  const child = fork(path.join(__dirname, 'environment.js'), [], {
    env: {
      PATH: process.env.PATH,
      DD_TRACE_AGENT_URL: agent.url,
      DD_FEATURE_FLAGS_EXTENSION_URL: extension.url,
      // No upstream poller in the SDK: Remote Config is off, and the
      // agentless URL (unused for this source) points at the trap.
      DD_FEATURE_FLAGS_CONFIGURATION_SOURCE: 'remote_config',
      DD_REMOTE_CONFIGURATION_ENABLED: 'false',
      DD_FEATURE_FLAGS_CONFIGURATION_SOURCE_AGENTLESS_BASE_URL: trap.url,
      DD_ENV: 'prod',
      DD_SERVICE: 'ffe-config-cache-example',
      DD_TRACE_STARTUP_LOGS: 'false',
      DD_INSTRUMENTATION_TELEMETRY_ENABLED: 'false',
      ...env,
    },
    stdio: ['ignore', 'ignore', 'inherit', 'ipc'],
  })
  cleanups.push(() => new Promise(resolve => {
    if (child.exitCode !== null || child.signalCode !== null) return resolve()
    child.once('exit', resolve)
    child.kill('SIGKILL')
  }))
  await new Promise((resolve, reject) => {
    child.once('message', resolve)
    child.once('exit', code => reject(new Error(`environment exited ${code}`)))
  })
  let next = 0
  const invoke = event => new Promise((resolve, reject) => {
    const id = next++
    const onMessage = message => {
      if (message.id !== id) return
      child.off('message', onMessage)
      message.error ? reject(new Error(message.error)) : resolve(message.result)
    }
    child.on('message', onMessage)
    child.send({ id, event })
  })
  return { invoke, child }
}

async function waitFor (predicate, timeoutMs, message) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (predicate()) return
    await sleep(25)
  }
  assert.fail(message())
}

async function fixtures () {
  return { cdn: await startCdn(), agent: await startAgent(), trap: await startTrap() }
}

const configRequests = cdn => cdn.requests.filter(r => r.path === FLAG_CONFIG_PATH)

test('the real SDK evaluates extension config and makes no upstream config request', async () => {
  const { cdn, agent, trap } = await fixtures()
  const extension = await startExtension(cdn)
  const env = await startEnvironment(extension, agent, trap)

  const us = await env.invoke(US_USER)
  const fr = await env.invoke(FR_USER)

  assert.equal(us.value, true)
  assert.equal(us.variant, 'on')
  assert.equal(fr.value, false)
  assert.equal(fr.variant, 'off')
  assert.equal(trap.hits, 0, 'the SDK contacted its agentless endpoint')
  assert.equal(agent.requests.filter(r => r.path.startsWith('/v0.7/config')).length, 0)
  const requests = configRequests(cdn)
  assert.equal(requests.length, 1, JSON.stringify(requests))
  assert.equal(requests[0].apiKey, 'stub-key')
  assert.equal(requests[0].env, 'prod')
})

test('warm invocations reuse the fresh cached config', async () => {
  const { cdn, agent, trap } = await fixtures()
  const extension = await startExtension(cdn)
  const env = await startEnvironment(extension, agent, trap)

  const statuses = []
  for (let i = 0; i < 50; i++) statuses.push((await env.invoke(US_USER)).config.status)

  assert.deepEqual(new Set(statuses.slice(1)), new Set([304]))
  assert.equal(configRequests(cdn).length, 1)
})

test('concurrent reads consolidate the upstream fetch', async () => {
  const { cdn } = await fixtures()
  cdn.delayMs = 300
  const extension = await startExtension(cdn)

  const replies = await Promise.all(Array.from({ length: 32 }, () =>
    fetch(extension.url + FLAG_CONFIG_PATH).then(async r => ({ status: r.status, body: await r.text() }))))

  for (const reply of replies) {
    assert.equal(reply.status, 200)
    assert.equal(reply.body, FIXTURES.v1)
  }
  // The Extension's startup prefetch and all 32 reads share one request.
  assert.equal(configRequests(cdn).length, 1)
})

test('an accepted update changes the real SDK evaluation', async () => {
  const { cdn, agent, trap } = await fixtures()
  const extension = await startExtension(cdn, { DD_FEATURE_FLAGS_EXTENSION_CACHE_TTL_SECONDS: '1' })
  const env = await startEnvironment(extension, agent, trap)
  assert.equal((await env.invoke(FR_USER)).value, false)

  cdn.mode = 'v2'
  // Idle past the TTL. The Extension runs no refresh timer, so for the cache
  // this is the same as a frozen environment: the next read sees the age.
  await sleep(1200)
  const after = await env.invoke(FR_USER)

  assert.equal(after.value, true)
  assert.equal(after.variant, 'on')
  assert.equal(after.config.status, 200)
  const requests = configRequests(cdn)
  assert.equal(requests.length, 2)
  assert.equal(requests[1].ifNoneMatch, '"v1"')
})

test('invalid and failed refreshes keep the accepted config until max stale', async () => {
  const { cdn, agent, trap } = await fixtures()
  const extension = await startExtension(cdn, {
    DD_FEATURE_FLAGS_EXTENSION_CACHE_TTL_SECONDS: '1',
    DD_FEATURE_FLAGS_EXTENSION_CACHE_MAX_STALE_SECONDS: '3',
  })
  const env = await startEnvironment(extension, agent, trap)
  assert.equal((await env.invoke(FR_USER)).value, false)

  cdn.mode = 'invalid'
  await sleep(1200)
  const corrupted = await env.invoke(FR_USER)
  assert.equal(corrupted.value, false)
  assert.equal(corrupted.config.cache, 'stale')

  cdn.mode = 'down'
  await sleep(2500)
  // Past max stale with Datadog down: the Extension stops serving the old
  // config (5xx), and the SDK keeps evaluating with the snapshot it has.
  const outage = await env.invoke(FR_USER)
  assert.ok(outage.config.status >= 500, `status ${outage.config.status}`)
  assert.equal(outage.value, false)
  assert.equal(outage.variant, 'off')
})

test('each fresh execution environment acquires its own config', async () => {
  const { cdn, agent, trap } = await fixtures()
  for (let i = 0; i < 3; i++) {
    const extension = await startExtension(cdn)
    const env = await startEnvironment(extension, agent, trap)
    assert.equal((await env.invoke(US_USER)).value, true)
    extension.child.kill('SIGKILL')
    env.child.kill('SIGKILL')
  }
  assert.equal(configRequests(cdn).length, 3)
})

test('a fresh bootstrap file replaces the initial acquisition', async () => {
  const { cdn, agent, trap } = await fixtures()
  const file = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'ffe-')), 'flags.json')
  const bootstrap = JSON.parse(FIXTURES.v1)
  bootstrap.data.attributes.createdAt = new Date().toISOString()
  fs.writeFileSync(file, JSON.stringify(bootstrap))
  const extension = await startExtension(cdn, { DD_FEATURE_FLAGS_EXTENSION_CACHE_BOOTSTRAP_FILE: file })
  const env = await startEnvironment(extension, agent, trap)

  const us = await env.invoke(US_USER)

  assert.equal(us.value, true)
  assert.equal(us.config.cache, 'fresh')
  assert.equal(configRequests(cdn).length, 0)
})

test('exposures are still reported through the trace agent', async () => {
  const { cdn, agent, trap } = await fixtures()
  const extension = await startExtension(cdn)
  const env = await startEnvironment(extension, agent, trap)

  await env.invoke(US_USER)

  const exposures = () => agent.requests.filter(r => r.path.includes('/api/v2/exposures'))
  await waitFor(() => exposures().length > 0, 10_000,
    () => `no exposure request: ${agent.requests.map(r => r.path).join(', ')}`)
  const body = exposures().map(r => r.body).join('\n')
  assert.match(body, /kill-switch/)
  assert.match(body, /user-1/)
})

test('zero code: the SDK poller reads the Extension, not the CDN', async () => {
  const { cdn, agent, trap } = await fixtures()
  const extension = await startExtension(cdn)
  const env = await startEnvironment(extension, agent, trap, {
    DD_FEATURE_FLAGS_CONFIGURATION_SOURCE: 'agentless',
    DD_FEATURE_FLAGS_CONFIGURATION_SOURCE_AGENTLESS_BASE_URL: extension.url,
    DD_FEATURE_FLAGS_CONFIGURATION_SOURCE_AGENTLESS_POLL_INTERVAL_SECONDS: '1',
  })

  // The SDK's first poll of the Extension makes the provider ready.
  let us
  for (let i = 0; i < 50 && us?.value !== true; i++) {
    us = await env.invoke(US_USER)
    await sleep(100)
  }
  assert.equal(us.value, true, JSON.stringify(us))
  // Three SDK polls of the Extension, still one CDN request.
  await sleep(3000)
  assert.equal(configRequests(cdn).length, 1)
  assert.equal(trap.hits, 0)
})
