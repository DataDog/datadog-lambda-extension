---
name: build-custom-layer
description: Build a custom Lambda extension layer from this repository and publish it to the serverless sandbox AWS account. Then make it public and give its ARN to a customer. The customer uses it to test a fix or collect debug logs for a support ticket. Use it when a support ticket needs a build that is not released.
---

# Build a custom extension layer for a support ticket

A custom layer lets a customer run a build of the extension that is not released, for example
a candidate fix or a build with more logs. Follow these steps in order.

## 1. Get the facts about the customer's function

Ask for these before you build. A layer that does not match the function does not work.

| Fact | Why it matters | Value to use |
| --- | --- | --- |
| AWS region of the function | A function can only use a layer from its own region. | `REGION`, for example `us-east-2` |
| Architecture of the function | The binary must match. `x86_64` in the Lambda console is `amd64` here. | `ARCHITECTURE=amd64` or `ARCHITECTURE=arm64` |
| FIPS or not | A customer who uses a FIPS layer, `Datadog-Extension-FIPS` or `Datadog-Extension-ARM-FIPS`, needs a FIPS build. | `FIPS=1` for a FIPS build, `FIPS=0` for all others |
| Extension version | Start from the version that the customer runs, so that the only change is the one for the ticket. | A tag such as `v100` |
| How the function is packaged | A layer works only for a function that is deployed as a .zip file. A function from a container image gets the extension from its image, and this skill does not cover it. | Continue only for a .zip function |

The sandbox account is a commercial AWS account. It cannot publish to AWS GovCloud or China
regions.

## 2. Prepare the code

Make a branch from the customer's version tag, and add the change for the ticket:

```sh
git switch -c <your-name>/<ticket>-custom-layer v100
# Make the change, for example cherry-pick the fix.
```

## 3. Build and publish the layer

Run this from the repository root. `SUFFIX` goes at the end of the layer name, so put the ticket
ID in it, so that the layer is easy to find later:

```sh
ARCHITECTURE=amd64 REGION=us-east-2 SUFFIX=<ticket-id> FIPS=0 ./scripts/publish_bottlecap_sandbox.sh
```

- The script builds the layer with `scripts/build_bottlecap_layer.sh` and publishes it with
  `aws-vault exec sso-serverless-sandbox-account-admin`. Docker must be running.
- The layer name is `Datadog-Bottlecap-Beta-<ticket-id>` for `amd64`, and
  `Datadog-Bottlecap-Beta-ARM-<ticket-id>` for `arm64`.
- The last line of the output gives the version number, for example
  `DONE: Published version 2 of layer Datadog-Bottlecap-Beta-<ticket-id> to region us-east-2`.
- `scripts/build_binary_and_layer_dockerized.sh` builds a different extension, the earlier Go
  agent. Do not use it for this.

## 4. Make the layer version public

The customer's AWS account must be able to read the layer. Use the layer name and the version
number from step 3:

```sh
aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda add-layer-version-permission \
  --region us-east-2 \
  --layer-name Datadog-Bottlecap-Beta-<ticket-id> \
  --version-number 2 \
  --statement-id public-access \
  --action lambda:GetLayerVersion \
  --principal '*'
```

`--principal '*'` lets every AWS account use this layer version. If the customer gives you their
AWS account ID, use that ID as the principal, so that only their account can use it.

## 5. Get the layer ARN

```sh
aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda get-layer-version \
  --region us-east-2 \
  --layer-name Datadog-Bottlecap-Beta-<ticket-id> \
  --version-number 2 \
  --query LayerVersionArn --output text
```

## 6. Send the ARN to the customer

Tell the customer to:

1. Replace the Datadog extension layer of the function with the ARN from step 5. They keep the
   other layers and all `DD_*` environment variables.
2. Set any environment variables that the ticket needs, for example `DD_LOG_LEVEL=debug`.
3. Run the function, and send back the result or the logs.
4. Change back to the released extension layer after the test.

## 7. Clean up after the ticket closes

Tell the customer before you remove access. Then remove the public permission:

```sh
aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda remove-layer-version-permission \
  --region us-east-2 \
  --layer-name Datadog-Bottlecap-Beta-<ticket-id> \
  --version-number 2 \
  --statement-id public-access
```
