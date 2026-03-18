"""
Durable execution Lambda handler for testing durable_function_execution_status tag.

This handler uses the @durable_execution decorator from the AWS Durable Execution SDK
to create a durable function. When invoked, the extension should detect the durable
execution status from the response and add the `durable_function_execution_status` tag
to the aws.lambda span.
"""
import json
import logging

from aws_durable_execution_sdk_python import (
    DurableContext,
    durable_execution,
    durable_step,
    StepContext,
)

logger = logging.getLogger()
logger.setLevel(logging.INFO)


@durable_step
def do_work(step_context: StepContext, message: str) -> str:
    """A simple durable step that returns the input message."""
    logger.info(f"Executing durable step with message: {message}")
    return f"Processed: {message}"


@durable_execution
def handler(event: dict, context: DurableContext) -> dict:
    """
    Durable Lambda handler.

    The @durable_execution decorator transforms this into a durable function.
    When invoked, AWS Lambda returns a response with {"Status": "SUCCEEDED|FAILED|PENDING", ...}
    which the extension should parse to set the durable_function_execution_status tag.
    """
    logger.info("Hello world!")

    message = event.get("message", "Hello from durable function!")

    # Execute a durable step - this creates a checkpoint
    result = context.step(do_work(message))

    logger.info(f"Durable step completed with result: {result}")

    return {
        "statusCode": 200,
        "body": json.dumps({
            "message": result,
        })
    }
