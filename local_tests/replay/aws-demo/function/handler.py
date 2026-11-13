"""Demo order handler: DynamoDB, S3 and a third-party HTTPS API."""
import json
import os
import urllib.request

import boto3

TABLE = os.environ["TABLE_NAME"]
BUCKET = os.environ["DATA_BUCKET"]

# Created at init, like most real functions.
dynamodb = boto3.client("dynamodb")
s3 = boto3.client("s3")


def handler(event, context):
    order_id = event["order_id"]
    dynamodb.put_item(
        TableName=TABLE,
        Item={"pk": {"S": order_id}, "qty": {"N": str(event["qty"])}},
    )
    item = dynamodb.get_item(TableName=TABLE, Key={"pk": {"S": order_id}})["Item"]
    s3.put_object(
        Bucket=BUCKET,
        Key=f"orders/{order_id}.json",
        Body=json.dumps(event).encode(),
        ContentType="application/json",
    )
    ip = urllib.request.urlopen("https://checkip.amazonaws.com", timeout=5).read()
    result = {
        "order_id": order_id,
        "qty": int(item["qty"]["N"]),
        "stored_at": f"s3://{BUCKET}/orders/{order_id}.json",
        "caller_ip_octets": len(ip.decode().strip().split(".")),
    }
    if os.environ.get("CHANGED_BEHAVIOR"):
        # Simulates a code change: the item now carries a status attribute.
        dynamodb.put_item(
            TableName=TABLE,
            Item={"pk": {"S": order_id}, "qty": {"N": str(event["qty"])}, "status": {"S": "new"}},
        )
    return result
