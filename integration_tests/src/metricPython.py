import time

from datadog_lambda.metric import lambda_metric

should_send_metric = true

def metric(event, context):
    if should_send_metric:
        lambda_metric(metric_name='serverless.lambda-extension.integration-test.count', value=1)
        should_send_metric = false
    return {
        "statusCode": 200,
        "body": "ok"
    }

def timeout(event, context):
    if should_send_metric:
        lambda_metric(metric_name='serverless.lambda-extension.integration-test.count', value=1)
        should_send_metric = false
    time.sleep(15 * 60)
    return {
        "statusCode": 200,
        "body": "ok"
    }
