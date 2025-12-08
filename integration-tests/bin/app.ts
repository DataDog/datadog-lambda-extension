#!/usr/bin/env node
import 'source-map-support/register';
import * as cdk from 'aws-cdk-lib';
import { BaseNodeStack } from '../lib/stacks/base-node-stack';
import { BasePythonStack } from '../lib/stacks/base-python-stack';
import { BaseJavaStack } from '../lib/stacks/base-java-stack';
import { BaseDotnetStack } from '../lib/stacks/base-dotnet-stack';
import { getIdentifier } from '../tests/utils/config';

const app = new cdk.App();

const env = {
  account: process.env.CDK_DEFAULT_ACCOUNT || process.env.AWS_ACCOUNT_ID,
  region: process.env.CDK_DEFAULT_REGION || process.env.AWS_REGION || 'us-east-1',
};

const identifier = getIdentifier();

new BaseNodeStack(app, `integ-${identifier}-base-node`, {
  env,
});

new BasePythonStack(app, `integ-${identifier}-base-python`, {
  env,
});

new BaseJavaStack(app, `integ-${identifier}-base-java`, {
  env,
});

new BaseDotnetStack(app, `integ-${identifier}-base-dotnet`, {
  env,
});

app.synth();
