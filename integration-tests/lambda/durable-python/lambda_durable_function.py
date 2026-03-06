import logging

logger = logging.getLogger()
logger.setLevel(logging.INFO)

def handler(event, context):
    logger.info('Hello from durable function!')
    # Return None rather than an HTTP-style dict; the Lambda durable execution
    # runtime rejects {'statusCode': 200} with "Invalid Status in invocation output."
