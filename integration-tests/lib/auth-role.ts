import * as cdk from 'aws-cdk-lib';
import * as iam from 'aws-cdk-lib/aws-iam';
import { Construct } from 'constructs';

/**
 * Stable IAM role name used by all auth integration test stacks.
 * The intake mapping is configured once for this role ARN.
 */
export const AUTH_ROLE_NAME = 'integ-auth-delegated-role';

export class AuthRoleStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props?: cdk.StackProps) {
    super(scope, id, props);

    const role = new iam.Role(this, 'AuthRole', {
      roleName: AUTH_ROLE_NAME,
      assumedBy: new iam.ServicePrincipal('lambda.amazonaws.com'),
      managedPolicies: [
        iam.ManagedPolicy.fromAwsManagedPolicyName('service-role/AWSLambdaBasicExecutionRole'),
      ],
    });

    new cdk.CfnOutput(this, 'RoleArn', {
      value: role.roleArn,
      description: 'Stable IAM Role ARN for auth integration tests - configure in intake mapping',
    });
  }
}
