import logging
import time

logger = logging.getLogger()
logger.setLevel(logging.INFO)

def handler(event, context):
    # Wait 10 seconds to guarantee concurrent execution overlap
    time.sleep(10)

    return {
        'statusCode': 200,
        'body': 'Snapstart Python function executed'
    }
