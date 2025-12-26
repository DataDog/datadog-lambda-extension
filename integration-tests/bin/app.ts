#!/usr/bin/env node
import 'source-map-support/register';
import * as cdk from 'aws-cdk-lib';
import {Base} from '../lib/stacks/base';
import {Otlp} from '../lib/stacks/otlp';
import {Snapstart} from '../lib/stacks/snapstart';
import {getIdentifier} from '../tests/utils/config';

const app = new cdk.App();

const env = {
    account: process.env.CDK_DEFAULT_ACCOUNT || process.env.AWS_ACCOUNT_ID,
    region: process.env.CDK_DEFAULT_REGION || process.env.AWS_REGION || 'us-east-1',
};

const identifier = getIdentifier();

const stacks = [
    new Base(app, `integ-${identifier}-base`, {
        env,
    }),
    new Otlp(app, `integ-${identifier}-otlp`, {
        env,
    }),
    new Snapstart(app, `integ-${identifier}-snapstart`, {
        env,
    }),
]

// Tag all stacks so we can easily clean them up
stacks.forEach(stack => stack.addStackTag("extension_integration_test", "true"))

app.synth();
