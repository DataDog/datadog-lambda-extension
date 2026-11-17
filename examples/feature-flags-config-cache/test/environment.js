'use strict'

// One simulated execution environment: loads the handler once, then runs the
// invocations the test sends over IPC.
const { handler } = require('../handler')

process.on('message', async ({ id, event }) => {
  try {
    process.send({ id, result: await handler(event) })
  } catch (error) {
    process.send({ id, error: String(error) })
  }
})
process.send({ ready: true })
