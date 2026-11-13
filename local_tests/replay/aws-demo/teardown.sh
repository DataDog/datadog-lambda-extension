#!/bin/bash
# Deletes every demo resource. Dry run by default; pass --apply to delete.
set -euo pipefail
source "$(dirname "$0")/config.sh"
APPLY=0; [ "${1:-}" = "--apply" ] && APPLY=1
run() { echo "+ $*"; if [ $APPLY = 1 ]; then "$@" || true; fi; }
run aws lambda delete-function --function-name "$FUNCTION"
for v in $(aws lambda list-layer-versions --layer-name "$LAYER" --query 'LayerVersions[].Version' --output text 2>/dev/null); do
  run aws lambda delete-layer-version --layer-name "$LAYER" --version-number "$v"
done
run aws iam delete-role-policy --role-name "$ROLE" --policy-name lambda-replay-demo
run aws iam detach-role-policy --role-name "$ROLE" --policy-arn arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole
run aws iam delete-role --role-name "$ROLE"
run aws dynamodb delete-table --table-name "$TABLE"
run aws s3 rm --recursive "s3://$BUCKET"
run aws s3api delete-bucket --bucket "$BUCKET"
[ $APPLY = 1 ] || echo "(dry run: pass --apply to delete)"
