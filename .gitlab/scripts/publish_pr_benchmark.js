#!/usr/bin/env node
'use strict'

const { execFileSync } = require('node:child_process')
const { createHash } = require('node:crypto')
const fs = require('node:fs')
const path = require('node:path')

const REPOSITORY = 'DataDog/datadog-lambda-extension'
const REGION = 'ca-central-1'
const ACCOUNT = '425362996713'
const STACK = 'PrBenchmark-dev-Shared'
const CATALOG = 'https://raw.githubusercontent.com/DataDog/serverless-plugin-datadog/main/src/layers.json'
const OUTPUT = '.layers/pr-benchmark'
const ARTIFACT = '.layers/datadog_extension-amd64.zip'
const hash = value => createHash('sha256').update(value).digest('hex')

function aws(...args) {
  return JSON.parse(execFileSync('aws', [...args, '--region', REGION, '--output', 'json'], {
    encoding: 'utf8', timeout: 120000, maxBuffer: 1024 * 1024
  }) || '{}')
}

async function download(url, env = process.env) {
  const headers = { 'User-Agent': 'datadog-lambda-extension-pr-benchmark' }
  const token = env.PR_BENCHMARK_GITHUB_TOKEN || env.GITHUB_TOKEN
  if (token && new URL(url).hostname === 'api.github.com') headers.Authorization = `Bearer ${token}`
  const response = await fetch(url, { headers, signal: AbortSignal.timeout(60000) })
  if (!response.ok) throw new Error(`Download failed (${response.status}) from ${new URL(url).hostname}`)
  return Buffer.from(await response.arrayBuffer())
}

async function resolvePr(sha, branch, json) {
  const matches = new Set()
  for (let page = 1; page <= 10; page++) {
    const pulls = await json(`https://api.github.com/repos/${REPOSITORY}/commits/${sha}/pulls?per_page=100&page=${page}`)
    if (!Array.isArray(pulls)) throw new Error('Unexpected GitHub PR response')
    for (const pr of pulls) {
      if (pr.state === 'open' && pr.base?.repo?.full_name === REPOSITORY &&
          pr.head?.repo?.full_name === REPOSITORY && pr.head.ref === branch) {
        if (!Number.isSafeInteger(pr.number) || pr.number < 1) throw new Error('Invalid GitHub PR number')
        matches.add(pr.number)
      }
    }
    if (pulls.length < 100) {
      if (matches.size > 1) throw new Error('Commit and branch match multiple open PRs')
      return matches.values().next().value
    }
  }
  throw new Error('PR lookup exceeded pagination limit')
}

function releasedNodeLayer(catalog) {
  const matches = [...new Set(Object.values(catalog.regions?.[REGION] || {}).filter(arn =>
    /^arn:aws:lambda:ca-central-1:464622532012:layer:Datadog-Node22-x:[1-9][0-9]*$/.test(arn)))]
  if (matches.length !== 1) throw new Error('Expected one released Node.js 22 layer')
  return matches[0]
}

function unzip(file, member) {
  const names = execFileSync('unzip', ['-Z1', file], { encoding: 'utf8', maxBuffer: 10 * 1024 * 1024 })
    .trim().split('\n').filter(name => name.replace(/^(\.\/|\/)+/, '') === member)
  if (names.length !== 1) throw new Error(`Expected exactly one ${member} in the layer`)
  return execFileSync('unzip', ['-p', file, names[0]], { maxBuffer: 100 * 1024 * 1024 })
}

function verifyExtension(binary) {
  if (binary.length < 20 || !binary.subarray(0, 6).equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46, 2, 1])) ||
      binary.readUInt16LE(18) !== 62) throw new Error('Expected a Linux x86_64 extension executable')
}

function nodeVersions(file) {
  const versions = {}
  for (const [key, name] of [['ddTrace', 'dd-trace'], ['lambdaJs', 'datadog-lambda-js']]) {
    const { version } = JSON.parse(unzip(file, `nodejs/node_modules/${name}/package.json`))
    if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error('Expected stable released Node component versions')
    versions[key] = version
  }
  return versions
}

async function publish(env = process.env, deps = {}) {
  const api = deps.aws || aws
  const bytes = deps.download || (url => download(url, env))
  const json = deps.json || (async url => JSON.parse(await bytes(url)))
  const now = deps.now || (() => Math.floor(Date.now() / 1000))
  const sha = env.PR_BENCHMARK_COMMIT_SHA
  const branch = env.PR_BENCHMARK_BRANCH
  fs.mkdirSync(OUTPUT, { recursive: true })
  const save = (name, value) => fs.writeFileSync(path.join(OUTPUT, name), JSON.stringify(value, null, 2) + '\n')
  if (env.PR_BENCHMARK_PIPELINE_SOURCE !== 'external_pull_request_event') return { skipped: 'Not a GitHub PR event' }
  if (!/^[a-f0-9]{40}$/.test(sha || '') || sha !== env.CI_COMMIT_SHA || !branch) {
    throw new Error('PR source SHA must match the pipeline that built the layer')
  }
  const prNumber = await resolvePr(sha, branch, json)
  save('resolution.json', { repository: REPOSITORY, commitSha: sha, branch, prNumber: prNumber || null })
  if (!prNumber) return { skipped: 'No matching open same-repository PR' }
  const artifact = fs.readFileSync(ARTIFACT)
  verifyExtension((deps.unzip || unzip)(ARTIFACT, 'extensions/datadog-agent'))
  if (api('sts', 'get-caller-identity').Account !== ACCOUNT) throw new Error('Wrong benchmark AWS account')
  const stack = api('cloudformation', 'describe-stacks', '--stack-name', STACK).Stacks[0]
  const outputs = Object.fromEntries(stack.Outputs.map(item => [item.OutputKey, item.OutputValue]))
  const target = JSON.parse(outputs.BenchmarkTargets)['datadog-lambda-extension']
  const days = Number(outputs.LayerRetentionDays)
  if (target?.mode !== 'queue' || target.extensionLayerName !== 'pr-benchmark-dev-extension-x86_64' ||
      !target.queueUrl?.startsWith(`https://sqs.${REGION}.amazonaws.com/${ACCOUNT}/`) ||
      !target.queueUrl.endsWith('.fifo') || !Number.isInteger(days) || days < 8 || days > 90) {
    throw new Error('Unexpected benchmark stack outputs')
  }
  const nodeArn = releasedNodeLayer(await json(CATALOG))
  const node = api('lambda', 'get-layer-version-by-arn', '--arn', nodeArn)
  if (node.CompatibleArchitectures && !node.CompatibleArchitectures.includes('x86_64')) {
    throw new Error('Released Node layer does not support x86_64')
  }
  const nodeFile = path.join(OUTPUT, 'node.zip')
  let versions
  try {
    fs.writeFileSync(nodeFile, await bytes(node.Content.Location))
    versions = (deps.nodeVersions || nodeVersions)(nodeFile)
  } finally {
    fs.rmSync(nodeFile, { force: true })
  }
  const builtAt = now()
  const expiresAt = builtAt + days * 86400
  const description = JSON.stringify({ managedBy: 'pr-benchmark', expiresAt, commitSha: sha })
  const layer = api('lambda', 'publish-layer-version', '--layer-name', target.extensionLayerName,
    '--zip-file', `fileb://${path.resolve(ARTIFACT)}`, '--compatible-architectures', 'x86_64',
    '--description', description)
  const prefix = `arn:aws:lambda:${REGION}:${ACCOUNT}:layer:${target.extensionLayerName}:`
  if (!layer.LayerVersionArn?.startsWith(prefix) || !/^[1-9][0-9]*$/.test(layer.LayerVersionArn.slice(prefix.length))) {
    throw new Error('Unexpected published extension ARN')
  }
  const message = {
    schemaVersion: 2, repository: REPOSITORY, name: `lambda-extension-pr-${prNumber}`, prNumber, commitSha: sha,
    runtime: 'nodejs22.x', architecture: 'x86_64', builtAt, expiresAt,
    layers: { node: nodeArn, extension: layer.LayerVersionArn },
    components: { ...versions, extension: sha }, packageVersions: versions,
    pipelineId: env.PR_BENCHMARK_BUILD_PIPELINE_ID, artifactSha256: hash(artifact)
  }
  // Keep the exact retained version recoverable if queue submission fails.
  save('job.json', message)
  api('sqs', 'send-message', '--queue-url', target.queueUrl,
    '--message-group-id', `datadog-lambda-extension-pr-${prNumber}`,
    '--message-deduplication-id', hash(JSON.stringify([REPOSITORY, prNumber, sha, nodeArn])),
    '--message-body', JSON.stringify(message))
  const result = { queued: message.name, commitSha: sha, layerVersionArn: layer.LayerVersionArn }
  save('publication.json', result)
  return result
}

module.exports = { publish, resolvePr, releasedNodeLayer, verifyExtension, nodeVersions }
if (require.main === module) publish().then(result => console.log(JSON.stringify(result))).catch(error => {
  console.error(error.message)
  process.exitCode = 1
})
