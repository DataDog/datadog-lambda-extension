import { channel } from 'node:diagnostics_channel'
import { createRequire } from 'node:module'
import { createInterface } from 'node:readline'
import { pathToFileURL } from 'node:url'
import { DatadogNodeServerProvider } from '@datadog/openfeature-node-server'
import { OpenFeature } from '@openfeature/server-sdk'

const require = createRequire(import.meta.url)
const exposures = []
const exposureChannel = channel('lambda-cache-prototype.exposures')
exposureChannel.subscribe((event) => exposures.push(event))

// This real Datadog provider accepts supplied rules. It has no CDN transport or poller.
const provider = new DatadogNodeServerProvider({ exposureChannel })
let registration
let revision
const sdk = {
  name: '@datadog/openfeature-node-server',
  version: require('@datadog/openfeature-node-server/package.json').version,
  resolved: require.resolve('@datadog/openfeature-node-server'),
}

export async function handler(event = {}) {
  registration ||= OpenFeature.setProviderAndWait(provider).catch(() => undefined)
  const endpoint = new URL('/api/v2/feature-flagging/config/rules-based/server', process.env.CONFIG_CACHE_URL)
  endpoint.searchParams.set('dd_env', process.env.DD_ENV || 'prod')
  let response
  try {
    response = await fetch(endpoint, {
      signal: AbortSignal.timeout(1000),
      headers: { 'X-Config-Budget-Ms': String(event.budgetMs || 100) },
    })
  } catch {
    // Evaluation still goes through the real provider when the extension is unavailable.
  }
  if (response?.ok) {
    const nextRevision = response.headers.get('x-config-version')
    const configuration = (await response.json()).data.attributes
    if (nextRevision !== revision) {
      provider.setConfiguration(configuration)
      revision = nextRevision
    }
  } else {
    // The extension owns the age limit. Do not keep using SDK rules it has expired.
    provider.setConfiguration(undefined)
    provider.setError(new Error('No usable configuration from the extension'))
    revision = undefined
  }
  await registration
  const details = await OpenFeature.getClient().getBooleanDetails('kill-switch', false, {
    targetingKey: event.targetingKey || 'lambda-user',
  })
  return {
    value: details.value,
    reason: details.reason,
    errorCode: details.errorCode,
    variant: details.variant,
    configurationVersion: revision,
    cacheSource: response?.headers.get('x-config-source'),
    sdk,
    exposures: exposures.splice(0),
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  for await (const line of createInterface({ input: process.stdin })) {
    try {
      console.log(JSON.stringify(await handler(JSON.parse(line))))
    } catch (error) {
      console.log(JSON.stringify({ error: error.message }))
    }
  }
  await OpenFeature.clearProviders()
}
