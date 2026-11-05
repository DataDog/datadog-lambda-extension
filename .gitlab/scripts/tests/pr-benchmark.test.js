const assert = require('node:assert/strict')
const { test } = require('node:test')
const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const { publish, resolvePr, releasedNodeLayer, verifyExtension } = require('../publish_pr_benchmark')

const SHA = 'a'.repeat(40)
const REPOSITORY = 'DataDog/datadog-lambda-extension'
const NODE = 'arn:aws:lambda:ca-central-1:464622532012:layer:Datadog-Node22-x:143'
const EXTENSION = 'arn:aws:lambda:ca-central-1:425362996713:layer:pr-benchmark-dev-extension-x86_64:7'
const QUEUE = 'https://sqs.ca-central-1.amazonaws.com/425362996713/extension.fifo'
const CATALOG = { regions: { 'ca-central-1': { node: NODE, other: 'unrelated' } } }
const ENV = { PR_BENCHMARK_PIPELINE_SOURCE: 'external_pull_request_event', PR_BENCHMARK_COMMIT_SHA: SHA,
  PR_BENCHMARK_BRANCH: 'feature', CI_COMMIT_SHA: SHA, PR_BENCHMARK_BUILD_PIPELINE_ID: '123' }
const pull = (number = 42, source = REPOSITORY) => ({ number, state: 'open', base: { repo: { full_name: REPOSITORY } },
  head: { ref: 'feature', sha: 'b'.repeat(40), repo: { full_name: source } } })
const elf = () => {
  const binary = Buffer.alloc(20)
  Buffer.from([0x7f, 0x45, 0x4c, 0x46, 2, 1]).copy(binary)
  binary.writeUInt16LE(62, 18)
  return binary
}

async function fixture(run, overrides = {}) {
  const original = process.cwd()
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'extension-pr-benchmark-'))
  const calls = []
  const target = { mode: 'queue', queueUrl: QUEUE, extensionLayerName: 'pr-benchmark-dev-extension-x86_64' }
  const api = (...args) => {
    calls.push(args)
    switch (args.slice(0, 2).join(' ')) {
      case 'sts get-caller-identity': return { Account: '425362996713' }
      case 'cloudformation describe-stacks': return { Stacks: [{ Outputs: [
        { OutputKey: 'BenchmarkTargets', OutputValue: JSON.stringify({ 'datadog-lambda-extension': target }) },
        { OutputKey: 'LayerRetentionDays', OutputValue: '14' }
      ] }] }
      case 'lambda get-layer-version-by-arn': return { Content: { Location: 'https://example.com/node.zip' }, CompatibleArchitectures: ['x86_64'] }
      case 'lambda publish-layer-version': return { LayerVersionArn: EXTENSION }
      case 'sqs send-message': return { MessageId: 'message' }
      default: throw new Error('Unexpected AWS request: ' + args)
    }
  }
  const deps = { aws: api, json: async url => url.includes('/commits/') ? [pull()] : CATALOG,
    download: async () => Buffer.from('node zip'), unzip: elf,
    nodeVersions: () => ({ ddTrace: '6.1.0', lambdaJs: '12.143.0' }), now: () => 1791560000, ...overrides }
  try {
    process.chdir(dir)
    fs.mkdirSync('.layers')
    fs.writeFileSync('.layers/datadog_extension-amd64.zip', 'built layer')
    await run({ deps, calls, api, target })
  } finally {
    process.chdir(original)
    fs.rmSync(dir, { recursive: true, force: true })
  }
}

test('an older source commit still resolves after the open PR head advances', async () => {
  assert.equal(await resolvePr(SHA, 'feature', async () => [pull()]), 42)
})

test('closed, fork, wrong-branch and wrong-target PRs do not qualify', async () => {
  const closed = { ...pull(), state: 'closed' }
  const otherBranch = pull(); otherBranch.head.ref = 'other'
  const otherTarget = pull(); otherTarget.base.repo.full_name = 'another/repo'
  assert.equal(await resolvePr(SHA, 'feature', async () => [closed, pull(43, 'contributor/fork'), otherBranch, otherTarget]), undefined)
})

test('ambiguity, API errors and pagination overflow fail visibly', async () => {
  await assert.rejects(resolvePr(SHA, 'feature', async () => [pull(), pull(43)]), /multiple/)
  await assert.rejects(resolvePr(SHA, 'feature', async () => { throw new Error('HTTP 403') }), /HTTP 403/)
  await assert.rejects(resolvePr(SHA, 'feature', async () => Array(100).fill(pull())), /pagination/)
})

test('pagination is exhausted before choosing a PR', async () => {
  const urls = []
  assert.equal(await resolvePr(SHA, 'feature', async url => {
    urls.push(url)
    return urls.length === 1 ? Array(100).fill({ ...pull(), state: 'closed' }) : [pull()]
  }), 42)
  assert.equal(urls.length, 2)
})

test('only one official released Node.js 22 layer is accepted', () => {
  assert.equal(releasedNodeLayer(CATALOG), NODE)
  assert.throws(() => releasedNodeLayer({ regions: {} }), /Expected one/)
  assert.throws(() => releasedNodeLayer({ regions: { 'ca-central-1': { a: NODE, b: NODE.replace(':143', ':144') } } }), /Expected one/)
})

test('the artifact must contain an x86_64 Linux ELF binary', () => {
  verifyExtension(elf())
  const arm = elf(); arm.writeUInt16LE(183, 18)
  assert.throws(() => verifyExtension(arm), /x86_64/)
  assert.throws(() => verifyExtension(Buffer.from('not an ELF')), /x86_64/)
})

test('publication pairs released Node components with the exact extension SHA and retains the layer', async () => {
  await fixture(async ({ deps, calls }) => {
    await publish(ENV, deps)
    const message = JSON.parse(fs.readFileSync('.layers/pr-benchmark/job.json'))
    assert.equal(message.name, 'lambda-extension-pr-42')
    assert.equal(message.schemaVersion, 2)
    assert.deepEqual(message.layers, { node: NODE, extension: EXTENSION })
    assert.deepEqual(message.components, { ddTrace: '6.1.0', lambdaJs: '12.143.0', extension: SHA })
    assert.equal(message.expiresAt - message.builtAt, 14 * 86400)
    const publication = calls.find(call => call[1] === 'publish-layer-version')
    assert.deepEqual(JSON.parse(publication[publication.indexOf('--description') + 1]), {
      managedBy: 'pr-benchmark', commitSha: SHA, expiresAt: message.expiresAt
    })
    const send = calls.find(call => call[0] === 'sqs')
    assert.equal(send[send.indexOf('--queue-url') + 1], QUEUE)
    assert.equal(send[send.indexOf('--message-group-id') + 1], 'datadog-lambda-extension-pr-42')
    assert.deepEqual(JSON.parse(send[send.indexOf('--message-body') + 1]), message)
    assert.equal(fs.existsSync('.layers/pr-benchmark/node.zip'), false)
  })
})

test('wrong pipeline SHA fails before AWS and unmatched PRs skip', async () => {
  await fixture(async ({ deps, calls }) => {
    await assert.rejects(publish({ ...ENV, CI_COMMIT_SHA: 'b'.repeat(40) }, deps), /source SHA/)
    assert.equal((await publish(ENV, { ...deps, json: async () => [] })).skipped, 'No matching open same-repository PR')
    assert.equal((await publish({ ...ENV, PR_BENCHMARK_PIPELINE_SOURCE: 'push' }, deps)).skipped, 'Not a GitHub PR event')
    assert.deepEqual(calls, [])
  })
})

test('wrong account and wrong destination cannot publish or send', async () => {
  await fixture(async ({ deps, calls, api, target }) => {
    await assert.rejects(publish(ENV, { ...deps, aws: (...args) => args[0] === 'sts' ? { Account: '111111111111' } : api(...args) }), /account/)
    target.queueUrl = 'https://sqs.us-east-1.amazonaws.com/425362996713/other.fifo'
    await assert.rejects(publish(ENV, deps), /outputs/)
    assert.equal(calls.some(call => call[1] === 'publish-layer-version' || call[0] === 'sqs'), false)
  })
})

test('a failed queue send preserves the exact retained manifest', async () => {
  await fixture(async ({ deps, api }) => {
    await assert.rejects(publish(ENV, { ...deps, aws: (...args) => {
      if (args[0] === 'sqs') throw new Error('SQS denied')
      return api(...args)
    } }), /SQS denied/)
    assert.equal(JSON.parse(fs.readFileSync('.layers/pr-benchmark/job.json')).layers.extension, EXTENSION)
    assert.equal(fs.existsSync('.layers/pr-benchmark/publication.json'), false)
  })
})

test('retry deduplication is stable even when a new layer version was published', async () => {
  await fixture(async ({ deps, calls, api }) => {
    await publish(ENV, deps)
    await publish(ENV, { ...deps, now: () => 1791560060, aws: (...args) => args[1] === 'publish-layer-version'
      ? { LayerVersionArn: EXTENSION.replace(':7', ':8') } : api(...args) })
    const sends = calls.filter(call => call[0] === 'sqs')
    assert.equal(sends[0][sends[0].indexOf('--message-deduplication-id') + 1],
      sends[1][sends[1].indexOf('--message-deduplication-id') + 1])
  })
})
