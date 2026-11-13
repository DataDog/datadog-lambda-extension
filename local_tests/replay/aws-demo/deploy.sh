#!/bin/bash
# Creates (or updates) the demo resources in the serverless sandbox account.
# Dry run by default: prints the AWS commands. Pass --apply to run them.
set -euo pipefail
source "$(dirname "$0")/config.sh"
APPLY=0; [ "${1:-}" = "--apply" ] && APPLY=1
run() { echo "+ $*"; if [ $APPLY = 1 ]; then "$@"; fi; }

ACTUAL=$(aws sts get-caller-identity --query Account --output text)
[ "$ACTUAL" = "$ACCOUNT" ] || { echo "credentials are for $ACTUAL, expected $ACCOUNT"; exit 1; }
[ -f "$LAYER_ZIP" ] || { echo "missing $LAYER_ZIP: run scripts/build_bottlecap_layer.sh"; exit 1; }

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/trust.json" <<JSON
{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole"}]}
JSON
cat > "$TMP/policy.json" <<JSON
{"Version":"2012-10-17","Statement":[
 {"Effect":"Allow","Action":["dynamodb:PutItem","dynamodb:GetItem"],"Resource":"arn:aws:dynamodb:$AWS_REGION:$ACCOUNT:table/$TABLE"},
 {"Effect":"Allow","Action":["s3:PutObject","s3:GetObject"],"Resource":"arn:aws:s3:::$BUCKET/*"}]}
JSON
(cd "$HERE/function" && zip -q -r "$TMP/function.zip" .)

if ! aws s3api head-bucket --bucket "$BUCKET" 2>/dev/null; then
  run aws s3api create-bucket --bucket "$BUCKET"
  run aws s3api put-public-access-block --bucket "$BUCKET" \
    --public-access-block-configuration BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true
fi
if ! aws dynamodb describe-table --table-name "$TABLE" >/dev/null 2>&1; then
  run aws dynamodb create-table --table-name "$TABLE" --billing-mode PAY_PER_REQUEST \
    --attribute-definitions AttributeName=pk,AttributeType=S --key-schema AttributeName=pk,KeyType=HASH
fi
if ! aws iam get-role --role-name "$ROLE" >/dev/null 2>&1; then
  run aws iam create-role --role-name "$ROLE" --assume-role-policy-document "file://$TMP/trust.json"
  run aws iam attach-role-policy --role-name "$ROLE" \
    --policy-arn arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole
  [ $APPLY = 1 ] && { echo "waiting for the role to propagate"; sleep 10; }
fi
run aws iam put-role-policy --role-name "$ROLE" --policy-name lambda-replay-demo --policy-document "file://$TMP/policy.json"

echo "+ aws lambda publish-layer-version --layer-name $LAYER --zip-file fileb://$LAYER_ZIP --compatible-architectures arm64"
LAYER_ARN="arn:aws:lambda:$AWS_REGION:$ACCOUNT:layer:$LAYER:<new version>"
if [ $APPLY = 1 ]; then
  LAYER_ARN=$(aws lambda publish-layer-version --layer-name "$LAYER" --zip-file "fileb://$LAYER_ZIP" \
    --compatible-architectures arm64 --query LayerVersionArn --output text)
fi

ENV="Variables={AWS_LAMBDA_EXEC_WRAPPER=/opt/datadog_wrapper,DD_REPLAY_RECORD=true,DD_REPLAY_S3_BUCKET=$BUCKET,DD_REPLAY_S3_PREFIX=recordings,TABLE_NAME=$TABLE,DATA_BUCKET=$BUCKET,DD_API_KEY=lambda-replay-demo-no-intake,DD_DD_URL=http://127.0.0.1:1,DD_URL=http://127.0.0.1:1,DD_APM_DD_URL=http://127.0.0.1:1,DD_LOGS_CONFIG_LOGS_DD_URL=http://127.0.0.1:1,DD_SERVERLESS_LOGS_ENABLED=false,DD_LOG_LEVEL=debug}"
if aws lambda get-function --function-name "$FUNCTION" >/dev/null 2>&1; then
  run aws lambda update-function-code --function-name "$FUNCTION" --zip-file "fileb://$TMP/function.zip"
  [ $APPLY = 1 ] && aws lambda wait function-updated --function-name "$FUNCTION"
  run aws lambda update-function-configuration --function-name "$FUNCTION" --layers "$LAYER_ARN" --environment "$ENV"
else
  run aws lambda create-function --function-name "$FUNCTION" --runtime python3.12 --architectures arm64 \
    --handler handler.handler --role "arn:aws:iam::$ACCOUNT:role/$ROLE" --zip-file "fileb://$TMP/function.zip" \
    --layers "$LAYER_ARN" --timeout 30 --memory-size 512 --environment "$ENV"
fi
[ $APPLY = 1 ] && aws lambda wait function-updated --function-name "$FUNCTION" && echo "deployed $FUNCTION"
[ $APPLY = 1 ] || echo "(dry run: pass --apply to run these)"
