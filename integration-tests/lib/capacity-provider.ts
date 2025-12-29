import * as cdk from 'aws-cdk-lib';
import { Construct } from 'constructs';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import * as ec2 from 'aws-cdk-lib/aws-ec2';
import * as iam from 'aws-cdk-lib/aws-iam';

/**
 * AWS Lambda Managed Instances Stack
 *
 * This stack demonstrates AWS Lambda Managed Instances with:
 * - VPC with NAT Gateway for Datadog Extension connectivity
 * - Capacity Provider with ARM64 architecture
 */
export class CapacityProviderStack extends cdk.Stack {
    constructor(scope: Construct, id: string, props?: cdk.StackProps) {
        super(scope, id, props);

        const vpc = new ec2.Vpc(this, `${id}-vpc`, {
            maxAzs: 3,
            natGateways: 1,
            subnetConfiguration: [
                {
                    name: 'Private',
                    subnetType: ec2.SubnetType.PRIVATE_WITH_EGRESS,
                    cidrMask: 24,
                },
                {
                    name: 'Public',
                    subnetType: ec2.SubnetType.PUBLIC,
                    cidrMask: 24,
                }
            ],
            enableDnsHostnames: true,
            enableDnsSupport: true
        });

        const securityGroup = new ec2.SecurityGroup(this, `${id}-security-group`, {
            vpc: vpc,
            description: 'Security group for Lambda Managed Instances',
            allowAllOutbound: false
        });

        // Allow HTTPS outbound for Datadog Extension and AWS services
        securityGroup.addEgressRule(
            ec2.Peer.anyIpv4(),
            ec2.Port.tcp(443),
            'Allow HTTPS to Datadog (*.datadoghq.com) and AWS services'
        );

        const operatorRole = new iam.Role(this, `${id}-operator-role`, {
            assumedBy: new iam.ServicePrincipal('lambda.amazonaws.com'),
            managedPolicies: [
                iam.ManagedPolicy.fromAwsManagedPolicyName('AWSLambdaManagedEC2ResourceOperator')
            ],
            description: 'Role for Lambda to manage EC2 instances in capacity provider'
        });

        const capacityProvider = new lambda.CapacityProvider(this, `${id}-cp`, {
            capacityProviderName: `${id}-cp`,
            subnets: vpc.selectSubnets({
                subnetType: ec2.SubnetType.PRIVATE_WITH_EGRESS
            }).subnets,
            securityGroups: [securityGroup],
            architectures: [lambda.Architecture.ARM_64],
            maxVCpuCount: 80,
            scalingOptions: lambda.ScalingOptions.auto(),
            operatorRole: operatorRole
        });

        new cdk.CfnOutput(this, 'VpcId', {
            value: vpc.vpcId,
            description: 'VPC ID'
        });
        new cdk.CfnOutput(this, 'CapacityProviderArn', {
            value: capacityProvider.capacityProviderArn,
            description: 'ARN of the Capacity Provider'
        });

    }
}
