import logging

logger = logging.getLogger()
logger.setLevel(logging.INFO)

def handler(event, context):
    logger.info('Hello World!')
    return {
        'statusCode': 200,
        'body': {
            'message': 'Success from Lambda Managed Instance',
            'requestId': context.aws_request_id
        }
    }
