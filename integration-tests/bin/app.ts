#!/usr/bin/env node
import 'source-map-support/register';
import * as cdk from 'aws-cdk-lib';
import {OnDemand} from '../lib/stacks/on-demand';
import {Otlp} from '../lib/stacks/otlp';
import {Snapstart} from '../lib/stacks/snapstart';
import {LambdaManagedInstancesStack} from '../lib/stacks/lmi';
import {AuthStack} from '../lib/stacks/auth';
import {Oom} from '../lib/stacks/oom';
import {LmiOom} from '../lib/stacks/lmi-oom';
import {CustomMetrics} from '../lib/stacks/custom-metrics';
import {PayloadSize} from '../lib/stacks/payload-size';
import {Dsm} from '../lib/stacks/dsm';
import {AuthRoleStack} from '../lib/auth-role';
import {ACCOUNT, IDENTIFIER, REGION} from '../config';
import {CapacityProviderStack} from "../lib/capacity-provider";

const app = new cdk.App();

const env = {
    account: ACCOUNT,
    region: REGION,
};

// Use the same Lambda Managed Instance Capacity Provider for all LMI functions.
// It is slow to create/destroy the related resources.
new CapacityProviderStack(app, `integ-default-capacity-provider`, {env});
new AuthRoleStack(app, `integ-auth-role`, {env});

const stacks = [
    new OnDemand(app, `${IDENTIFIER}-on-demand`, {
        env,
    }),
    new Otlp(app, `${IDENTIFIER}-otlp`, {
        env,
    }),
    new Snapstart(app, `${IDENTIFIER}-snapstart`, {
        env,
    }),
    new LambdaManagedInstancesStack(app, `${IDENTIFIER}-lmi`, {
        env,
    }),
    new AuthStack(app, `${IDENTIFIER}-auth`, {
        env,
    }),
    new Oom(app, `${IDENTIFIER}-oom`, {
        env,
    }),
    new LmiOom(app, `${IDENTIFIER}-lmi-oom`, {
        env,
    }),
    new CustomMetrics(app, `${IDENTIFIER}-custom-metrics`, {
        env,
    }),
    new PayloadSize(app, `${IDENTIFIER}-payload-size`, {
        env,
    }),
    new Dsm(app, `integ-${identifier}-dsm`, {
        env,
    }),
]

// Tag all stacks so we can easily clean them up
stacks.forEach(stack => stack.addStackTag("extension_integration_test", "true"))

app.synth();
