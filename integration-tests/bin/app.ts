#!/usr/bin/env node
import 'source-map-support/register';
import * as cdk from 'aws-cdk-lib';
import {BaseNodeStack} from '../lib/stacks/base-node-stack';
import {BasePythonStack} from '../lib/stacks/base-python-stack';
import {BaseJavaStack} from '../lib/stacks/base-java-stack';
import {BaseDotnetStack} from '../lib/stacks/base-dotnet-stack';
import {OtlpNodeStack} from '../lib/stacks/otlp-node-stack';
import {OtlpPythonStack} from '../lib/stacks/otlp-python-stack';
import {OtlpJavaStack} from '../lib/stacks/otlp-java-stack';
import {OtlpDotnetStack} from '../lib/stacks/otlp-dotnet-stack';
import {getIdentifier} from '../tests/utils/config';

const app = new cdk.App();

const env = {
    account: process.env.CDK_DEFAULT_ACCOUNT || process.env.AWS_ACCOUNT_ID,
    region: process.env.CDK_DEFAULT_REGION || process.env.AWS_REGION || 'us-east-1',
};

const identifier = getIdentifier();

const stacks = [
    new BaseNodeStack(app, `integ-${identifier}-base-node`, {
        env,
    }),
    new BasePythonStack(app, `integ-${identifier}-base-python`, {
        env,
    }),
    new BaseJavaStack(app, `integ-${identifier}-base-java`, {
        env,
    }),
    new BaseDotnetStack(app, `integ-${identifier}-base-dotnet`, {
        env,
    }),
    new OtlpNodeStack(app, `integ-${identifier}-otlp-node`, {
        env,
    }),
    new OtlpPythonStack(app, `integ-${identifier}-otlp-python`, {
        env,
    }),
    new OtlpJavaStack(app, `integ-${identifier}-otlp-java`, {
        env,
    }),
    new OtlpDotnetStack(app, `integ-${identifier}-otlp-dotnet`, {
        env,
    }),
]

// Tag all stacks so we can easily clean them up
stacks.forEach(stack => stack.addStackTag("extension_integration_test", "true"))

app.synth();
