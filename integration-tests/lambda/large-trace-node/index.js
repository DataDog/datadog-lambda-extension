'use strict';

const tracer = require('dd-trace');

/**
 * Models a service that processes a batch of records and captures the payload
 * it handles on each span, producing one large single-invocation trace
 * (~spanCount * payloadBytes) to exercise the extension's per-batch size cap.
 * Configured entirely via the invocation event: `spanCount` (keep under 1000 so
 * the tracer keeps all spans in one trace) and `payloadBytes` (capped at the
 * tracer's 25,000-char tag-value limit).
 */

const MAX_TAG_VALUE_BYTES = 24000; // under the tracer's 25,000-char truncation limit

/** Builds an order document of roughly `targetBytes`, sized via line items. */
function buildOrderPayload(targetBytes, requestId, index) {
  const order = {
    orderId: `ord-${requestId}-${index}`,
    placedAt: '2026-06-11T10:00:00.000Z',
    channel: index % 2 === 0 ? 'web' : 'mobile',
    customer: {
      tier: index % 3 === 0 ? 'enterprise' : 'standard',
      shippingAddress: '123 Example Street, Suite 400, Sydney NSW 2000, Australia',
    },
    lineItems: [],
  };
  const makeItem = (item) => ({
    sku: `SKU-${(item * 7 + index) % 100000}`,
    description: `Wholesale item ${item} — replenishment for warehouse wh-${item % 40}`,
    quantity: (item % 12) + 1,
    unitPriceCents: 1999 + (item % 500),
  });
  const baseLen = JSON.stringify(order).length;
  const itemLen = JSON.stringify(makeItem(0)).length + 1; // + array comma
  const itemCount = Math.max(0, Math.ceil((targetBytes - baseLen) / itemLen));
  for (let item = 0; item < itemCount; item++) {
    order.lineItems.push(makeItem(item));
  }
  return JSON.stringify(order).slice(0, targetBytes);
}

exports.handler = async (event, context) => {
  const spanCount = Number(event?.spanCount ?? 400);
  const payloadBytes = Math.min(Number(event?.payloadBytes ?? 24000), MAX_TAG_VALUE_BYTES);
  console.log(`Processing batch of ${spanCount} records (~${payloadBytes} bytes captured per span)`);

  for (let i = 0; i < spanCount; i++) {
    const requestBody = buildOrderPayload(payloadBytes, context.awsRequestId, i);
    await tracer.trace(
      'order.process',
      { resource: `POST /orders/batch[${i}]`, service: process.env.DD_SERVICE },
      async (span) => {
        // Captured payloads dominate the span's size.
        span.addTags({
          'http.method': 'POST',
          'http.route': '/orders',
          'http.status_code': i % 50 === 0 ? 500 : 200,
          'order.id': `ord-${context.awsRequestId}-${i}`,
          'order.request_body': requestBody,
          'order.response_body': `{"orderId":"ord-${context.awsRequestId}-${i}","status":"accepted"}`,
        });
      },
    );
  }

  console.log(`Finished processing ${spanCount} records`);

  return {
    statusCode: 200,
    body: JSON.stringify({
      message: 'Success',
      requestId: context.awsRequestId,
      recordsProcessed: spanCount,
    }),
  };
};
