find_all_regions() {
  aws-vault exec sso-prod-engineering -- aws ec2 describe-regions | jq -r '.[] | .[] | .RegionName' >all_regions.txt
}

find_layers_from_all_regions() {
  while IFS= read -r region; do
    aws-vault exec sso-prod-engineering -- aws lambda list-layers --region "$region" | grep LayerVersionArn | cut -d':' -f6- >all_layers_"$region".log
  done <all_regions.txt
}

diff_from_useast1() {
  for f1 in all_layers_*.log; do
    diff --unified=0 "$f1" all_layers_us-east-1.log
  done | grep -vE "@@|Datadog-Trace-Forwarder-Python|Node8|metric|Node10|Node12|Python27|Python36|Ruby2-5" >all_diff.log # ignore deprecated layers
}

"$@"
