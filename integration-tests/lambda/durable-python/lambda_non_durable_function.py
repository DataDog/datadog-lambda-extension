import logging

logger = logging.getLogger()
logger.setLevel(logging.INFO)

def handler(event, context):
    logger.info('Hello from non-durable function!')
    return {
        'statusCode': 200
    }
