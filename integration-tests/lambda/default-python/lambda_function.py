import json
import logging

logger = logging.getLogger()
logger.setLevel(logging.INFO)


def handler(event, context):
    logger.info('Hello world!')

    return {
        'statusCode': 200,
        'body': json.dumps({
            'message': 'Success',
            'requestId': context.aws_request_id
        })
    }
