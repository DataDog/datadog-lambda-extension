import logging

logger = logging.getLogger()
logger.setLevel(logging.INFO)

def handler(event, context):
    logger.info('Dynamic instrumentation test function.')
    result = process_data(event)
    return {
        'statusCode': 200,
        'body': result
    }

def process_data(data):
    value = data.get('value', 0)
    logger.info(f'Processing data with value: {value}')
    multiplied = value * 2
    return f'Processed: {multiplied}'
