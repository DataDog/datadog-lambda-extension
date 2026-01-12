# Releasing the Datadog Lambda Extension

This guide covers how to build and publish the Lambda extension layer to AWS.

## Prerequisites

### Step 1: Create GitHub OIDC Identity Provider in AWS

1. Go to **IAM → Identity providers → Add provider**

2. Configure the provider:
   - **Provider type:** OpenID Connect
   - **Provider URL:** `https://token.actions.githubusercontent.com`
   - **Audience:** `sts.amazonaws.com`

3. Click **Add provider**

Or via AWS CLI:

```bash
aws iam create-open-id-connect-provider \
  --url https://token.actions.githubusercontent.com \
  --client-id-list sts.amazonaws.com
```

### Step 2: Create IAM Role for GitHub Actions

1. Go to **IAM → Roles → Create role**

2. Select **Web identity** as the trusted entity type

3. Configure:
   - **Identity provider:** `token.actions.githubusercontent.com`
   - **Audience:** `sts.amazonaws.com`

4. Click **Next**, then **Create policy** (in a new tab) with this JSON:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": ["lambda:PublishLayerVersion", "lambda:ListLayerVersions"],
      "Resource": "arn:aws:lambda:*:YOUR_ACCOUNT_ID:layer:Datadog-Extension*"
    },
    {
      "Effect": "Allow",
      "Action": "sts:GetCallerIdentity",
      "Resource": "*"
    }
  ]
}
```

5. Name the policy `LambdaLayerPublish` and create it

6. Back in the role creation, attach the `LambdaLayerPublish` policy

7. Name the role (e.g., `GitHubActionsLambdaLayerPublish`) and create it

8. **Edit the trust policy** to restrict to your repository. Go to the role →
   Trust relationships → Edit:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Principal": {
        "Federated": "arn:aws:iam::YOUR_ACCOUNT_ID:oidc-provider/token.actions.githubusercontent.com"
      },
      "Action": "sts:AssumeRoleWithWebIdentity",
      "Condition": {
        "StringEquals": {
          "token.actions.githubusercontent.com:aud": "sts.amazonaws.com"
        },
        "StringLike": {
          "token.actions.githubusercontent.com:sub": "repo:YOUR_ORG/YOUR_REPO:*"
        }
      }
    }
  ]
}
```

Replace:

- `YOUR_ACCOUNT_ID` with your AWS account ID
- `YOUR_ORG/YOUR_REPO` with your GitHub org and repo (e.g.,
  `usetero/datadog-lambda-extension`)

### Step 3: Add Role ARN to GitHub Secrets

1. Copy the role ARN (e.g.,
   `arn:aws:iam::123456789012:role/GitHubActionsLambdaLayerPublish`)

2. Go to your GitHub repository **Settings → Secrets and variables → Actions**

3. Add a new secret:
   - **Name:** `AWS_ROLE_ARN`
   - **Value:** Your role ARN

## Releasing via GitHub Actions

### Option 1: Manual Release (Recommended)

1. Go to **Actions → Release Lambda Extension → Run workflow**

2. Configure the release options:

   | Option          | Description                                          | Example                         |
   | --------------- | ---------------------------------------------------- | ------------------------------- |
   | `version`       | Version number (auto-increments if empty)            | `100`                           |
   | `layer_suffix`  | Suffix to differentiate from official Datadog layers | `-Tero`                         |
   | `regions`       | Comma-separated AWS regions                          | `us-east-1,us-west-2,eu-west-1` |
   | `architectures` | Which architectures to build                         | `amd64,arm64`                   |
   | `fips`          | Include FIPS-compliant builds                        | `false`                         |
   | `dry_run`       | Build without publishing to AWS                      | `false`                         |

3. Click **Run workflow**

4. After completion, find the Layer ARNs in the workflow summary

### Option 2: Tag-based Release

Push a version tag to trigger an automatic release:

```bash
git tag v100
git push origin v100
```

This publishes to the default region (us-east-1) with both architectures.

## Layer Naming Convention

The workflow creates layers with this naming pattern:

| Architecture | FIPS | Suffix  | Layer Name                        |
| ------------ | ---- | ------- | --------------------------------- |
| amd64        | No   | `-Tero` | `Datadog-Extension-Tero`          |
| arm64        | No   | `-Tero` | `Datadog-Extension-ARM-Tero`      |
| amd64        | Yes  | `-Tero` | `Datadog-Extension-FIPS-Tero`     |
| arm64        | Yes  | `-Tero` | `Datadog-Extension-ARM-FIPS-Tero` |

## Using the Published Layer

### Find your Layer ARN

After publishing, the Layer ARN follows this format:

```
arn:aws:lambda:REGION:ACCOUNT_ID:layer:LAYER_NAME:VERSION
```

Example:

```
arn:aws:lambda:us-east-1:123456789012:layer:Datadog-Extension-ARM-Tero:1
```

### Update a Lambda function

**AWS CLI:**

```bash
aws lambda update-function-configuration \
  --function-name my-function \
  --layers "arn:aws:lambda:us-east-1:123456789012:layer:Datadog-Extension-ARM-Tero:1"
```

**Terraform:**

```hcl
resource "aws_lambda_function" "example" {
  # ... other configuration ...

  layers = [
    "arn:aws:lambda:us-east-1:123456789012:layer:Datadog-Extension-ARM-Tero:1"
  ]
}
```

**AWS SAM:**

```yaml
MyFunction:
  Type: AWS::Serverless::Function
  Properties:
    Layers:
      - arn:aws:lambda:us-east-1:123456789012:layer:Datadog-Extension-ARM-Tero:1
```

**Serverless Framework:**

```yaml
functions:
  myFunction:
    layers:
      - arn:aws:lambda:us-east-1:123456789012:layer:Datadog-Extension-ARM-Tero:1
```

## Local Build and Manual Publish

If you need to build and publish manually without GitHub Actions:

### Build the layer

```bash
# For ARM64
ARCHITECTURE=arm64 FIPS=0 ALPINE=0 DEBUG=0 ./scripts/build_bottlecap_layer.sh

# For AMD64
ARCHITECTURE=amd64 FIPS=0 ALPINE=0 DEBUG=0 ./scripts/build_bottlecap_layer.sh
```

The layer zip will be created in `.layers/`.

### Publish to AWS

```bash
# Set your region
export AWS_REGION=us-east-1

# Publish ARM64 layer
aws lambda publish-layer-version \
  --layer-name "Datadog-Extension-ARM-Tero" \
  --description "Tero fork of Datadog Lambda Extension" \
  --zip-file "fileb://.layers/datadog_extension-arm64.zip" \
  --compatible-architectures arm64 \
  --region $AWS_REGION

# Publish AMD64 layer
aws lambda publish-layer-version \
  --layer-name "Datadog-Extension-Tero" \
  --description "Tero fork of Datadog Lambda Extension" \
  --zip-file "fileb://.layers/datadog_extension-amd64.zip" \
  --compatible-architectures x86_64 \
  --region $AWS_REGION
```

## Troubleshooting

### Build fails with Boost download error

The Boost download URL may change. Check `images/Dockerfile.bottlecap.compile`
and update the SourceForge URL if needed.

### Build fails with policy-rs compilation error on ARM64

Ensure you're using `policy-rs` version 1.1.1 or later, which includes the ARM64
compatibility fix.

### Layer version already exists

The workflow skips publishing if the target version already exists. Either:

- Let the version auto-increment (leave version field empty)
- Specify a higher version number

### Permission denied when publishing

Verify your IAM role has the required `lambda:PublishLayerVersion` permission
and the trust policy allows your repository.

### OIDC authentication fails

1. Verify the OIDC provider exists in IAM → Identity providers
2. Check the trust policy has the correct `sub` claim for your repo
3. Ensure `AWS_ROLE_ARN` secret is set correctly in GitHub

## Multi-Region Deployment

To deploy to multiple regions, specify them comma-separated:

```
us-east-1,us-west-2,eu-west-1,ap-southeast-1
```

Each region will get its own copy of the layer. Lambda functions must use a
layer from the same region they're deployed in.
