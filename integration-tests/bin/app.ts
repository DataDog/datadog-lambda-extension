#!/usr/bin/env node
import 'source-map-support/register';
import * as cdk from 'aws-cdk-lib';
import * as os from 'os';
import { ExampleTestStack } from '../lib/example-test-stack';

const app = new cdk.App();

// Get configuration from context or environment variables
const env = {
  account: process.env.CDK_DEFAULT_ACCOUNT || process.env.AWS_ACCOUNT_ID,
  region: process.env.CDK_DEFAULT_REGION || process.env.AWS_REGION || 'us-east-1',
};

// Get suffix from environment variable, or derive from username, or default
function getIdentifier(): string {
  if (process.env.SUFFIX) {
    return process.env.SUFFIX;
  }
  
  try {
    const username = os.userInfo().username;
    const firstName = username.split('.')[0];
    if (firstName && firstName.length > 0) {
      return firstName;
    }
  } catch (error) {
  }
  
  return 'integration';
}

const identifier = getIdentifier();

new ExampleTestStack(app, `IntegrationTests-${identifier}-ExampleTestStack`, {
  env,
  identifier,
});

app.synth();
