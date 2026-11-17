'use strict'

// Runs once per execution environment (the Lambda Init phase).
const tracer = require('dd-trace').init()
const { OpenFeature } = require('@openfeature/server-sdk')
const { ExtensionConfig } = require('./extension-config')

const provider = tracer.openfeature
// Resolves once the provider has a configuration. OpenFeature marks the
// client ready asynchronously, after `setConfiguration` returns.
const ready = OpenFeature.setProviderAndWait(provider).catch(() => {})
const client = OpenFeature.getClient()

// With DD_FEATURE_FLAGS_CONFIGURATION_SOURCE=agentless and the base URL set
// to the Extension, the tracer polls the Extension on its own timer. Otherwise
// the handler reloads from the Extension at the start of each invocation.
const extensionConfig = process.env.DD_FEATURE_FLAGS_CONFIGURATION_SOURCE === 'agentless'
  ? undefined
  : new ExtensionConfig(provider)

exports.handler = async (event) => {
  const config = await extensionConfig?.reload()
  if (extensionConfig?.etag) await ready
  const details = await client.getBooleanDetails('kill-switch', false, {
    targetingKey: event.userId,
    country: event.country,
  })
  return { value: details.value, variant: details.variant, reason: details.reason, config }
}
