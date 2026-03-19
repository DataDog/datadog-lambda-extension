#!/usr/bin/env node
import 'source-map-support/register';
import * as cdk from 'aws-cdk-lib';
import {OnDemand} from '../lib/stacks/on-demand';
import {Otlp} from '../lib/stacks/otlp';
import {Snapstart} from '../lib/stacks/snapstart';
import {LambdaManagedInstancesStack} from '../lib/stacks/lmi';
import {DurableExecutionStatusStack} from '../lib/stacks/de-status';
import {ACCOUNT, getIdentifier, REGION} from '../config';
import {CapacityProviderStack} from "../lib/capacity-provider";

const app = new cdk.App();

const env = {
    account: ACCOUNT,
    region: REGION,
};

const identifier = getIdentifier();

// Use the same Lambda Managed Instance Capacity Provider for all LMI functions.
// It is slow to create/destroy the related resources.
new CapacityProviderStack(app, `integ-default-capacity-provider`, {env});

const stacks = [
    new OnDemand(app, `integ-${identifier}-on-demand`, {
        env,
    }),
    new Otlp(app, `integ-${identifier}-otlp`, {
        env,
    }),
    new Snapstart(app, `integ-${identifier}-snapstart`, {
        env,
    }),
    new LambdaManagedInstancesStack(app, `integ-${identifier}-lmi`, {
        env,
    }),
    new DurableExecutionStatusStack(app, `integ-${identifier}-de-status`, {
        env,
    }),
]

// Tag all stacks so we can easily clean them up
stacks.forEach(stack => stack.addStackTag("extension_integration_test", "true"))

app.synth();
