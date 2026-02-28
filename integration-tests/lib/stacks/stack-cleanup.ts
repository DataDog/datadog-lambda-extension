import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import * as iam from 'aws-cdk-lib/aws-iam';
import * as events from 'aws-cdk-lib/aws-events';
import * as targets from 'aws-cdk-lib/aws-events-targets';
import { Construct } from 'constructs';
import { createLogGroup } from '../util';

export class StackCleanup extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const functionName = `${id}-cleanup-lambda`;

    const cleanupFunction = new lambda.Function(this, functionName, {
      runtime: lambda.Runtime.NODEJS_24_X,
      architecture: lambda.Architecture.ARM_64,
      handler: 'index.handler',
      code: lambda.Code.fromAsset('./lambda/stack-cleanup'),
      functionName: functionName,
      timeout: cdk.Duration.minutes(15),
      memorySize: 512,
      environment: {
        TS: Date.now().toString()
      },
      logGroup: createLogGroup(this, functionName)
    });

    cleanupFunction.addToRolePolicy(new iam.PolicyStatement({
      effect: iam.Effect.ALLOW,
      actions: [
        'cloudformation:ListStacks',
        'cloudformation:DescribeStacks',
        'cloudformation:DeleteStack',
        'cloudformation:DescribeStackEvents',
        'cloudformation:DescribeStackResources',
        'lambda:DeleteFunction',
        'lambda:RemovePermission',
        'logs:DeleteLogGroup',
        'logs:DeleteRetentionPolicy',
        'iam:DeleteRole',
        'iam:DeleteRolePolicy',
        'iam:DetachRolePolicy',
        'iam:GetRole',
        'iam:GetRolePolicy',
        'iam:ListRolePolicies',
        'iam:ListAttachedRolePolicies'
      ],
      resources: ['*']
    }));

    const rule = new events.Rule(this, 'DailyCleanupRule', {
      schedule: events.Schedule.rate(cdk.Duration.days(1)),
      description: 'Triggers stack cleanup Lambda once per day'
    });
    rule.addTarget(new targets.LambdaFunction(cleanupFunction));

  }
}
