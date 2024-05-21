import time
from ddtrace import tracer
from datadog_lambda.metric import lambda_metric

def handler(event, context):
    # submit a custom span
    with tracer.trace("self.monitoring"):
        time.sleep(0.1)

    X = 10
    Y = 0
    print(X/Y)

    # submit a custom metric
    lambda_metric(metric_name='self.monitoring', value=1)

    return {
        "statusCode": 200,
        "body": "hello, world"
    }