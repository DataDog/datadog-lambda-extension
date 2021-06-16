import time
from random import random

from datadog_lambda.metric import lambda_metric

def metric(event, context):
    lambda_metric(metric_name='serverless.lambda-extension.integration-test.count' + str(random()), value=1)
    return {
        "statusCode": 200,
        "body": "ok"
    }

def timeout(event, context):
    lambda_metric(metric_name='serverless.lambda-extension.integration-test.count' + str(random()), value=1)
    time.sleep(15 * 60)
    return {
        "statusCode": 200,
        "body": "ok"
    }
