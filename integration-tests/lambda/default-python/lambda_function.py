import json
import logging
import os
import time

logger = logging.getLogger()
logger.setLevel(logging.INFO)


def handler(event, context):
    logger.info('Hello world!')

    sleep_ms = int(os.environ.get('SLEEP_MS', '0'))
    if sleep_ms > 0:
        logger.info(f'Sleeping for {sleep_ms}ms')
        time.sleep(sleep_ms / 1000)

    return {
        'statusCode': 200,
        'body': json.dumps({
            'message': 'Success',
            'requestId': context.aws_request_id
        })
    }
