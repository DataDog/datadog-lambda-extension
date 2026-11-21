'use strict'

const http = require('node:http')

const FLAG_CONFIG_PATH = '/api/v2/feature-flagging/config/rules-based/server'

/**
 * Loads the flag configuration from the Datadog Extension into the tracer's
 * flag provider. Call `reload()` at the start of each invocation: the
 * Extension answers from its cache (or `304` when nothing changed), and only
 * the Extension contacts Datadog.
 *
 * Prototype glue: `setConfiguration` exists on `tracer.openfeature` at runtime
 * but is not in dd-trace's public types, and unwrapping the envelope repeats
 * what dd-trace's private agentless source does.
 */
class ExtensionConfig {
  /**
   * @param {{ setConfiguration: (configuration: object) => void }} provider
   * @param {{ url?: string, timeoutMs?: number }} [options]
   */
  constructor (provider, options = {}) {
    this.provider = provider
    this.url = (options.url ?? process.env.DD_FEATURE_FLAGS_EXTENSION_URL ?? 'http://127.0.0.1:8126') +
      FLAG_CONFIG_PATH
    // Above the Extension's startup wait, so the Extension decides the outcome.
    this.timeoutMs = options.timeoutMs ?? 3500
    this.etag = undefined
    this.last = undefined
  }

  async reload () {
    const reply = await get(this.url, this.etag, this.timeoutMs)
    this.last = { status: reply.status, cache: reply.headers['x-datadog-flag-config-cache'] }
    if (reply.status !== 200) return this.last

    try {
      // The Extension relays the CDN's JSON:API envelope.
      const { data } = JSON.parse(reply.body)
      if (data?.type !== 'universal-flag-configuration') throw new Error('not a flag configuration')
      this.provider.setConfiguration(data.attributes)
      this.etag = reply.headers.etag
    } catch {
      // Keep the configuration the provider already has.
      this.last.status = 'rejected'
    }
    return this.last
  }
}

function get (url, etag, timeoutMs) {
  return new Promise(resolve => {
    const headers = etag ? { 'If-None-Match': etag } : {}
    const req = http.get(url, { headers, timeout: timeoutMs }, res => {
      let body = ''
      res.setEncoding('utf8')
      res.on('data', chunk => { body += chunk })
      res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, body }))
    })
    req.on('timeout', () => req.destroy(new Error('timeout')))
    req.on('error', () => resolve({ status: 0, headers: {}, body: '' }))
  })
}

module.exports = { ExtensionConfig, FLAG_CONFIG_PATH }
