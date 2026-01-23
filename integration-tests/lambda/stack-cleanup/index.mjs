import { CloudFormationClient, ListStacksCommand, DescribeStacksCommand, DeleteStackCommand } from "@aws-sdk/client-cloudformation";

const client = new CloudFormationClient({});

const SEVEN_DAYS_MS = 7 * 24 * 60 * 60 * 1000;

export const handler = async (event) => {
  console.log('Starting stack cleanup process...');

  const cutoffTime = Date.now() - SEVEN_DAYS_MS;
  const stacksToDelete = [];

  try {
    const listCommand = new ListStacksCommand({
      StackStatusFilter: [
        'CREATE_COMPLETE',
        'ROLLBACK_COMPLETE',
        'UPDATE_COMPLETE',
        'UPDATE_ROLLBACK_COMPLETE',
        'IMPORT_COMPLETE',
        'IMPORT_ROLLBACK_COMPLETE'
      ]
    });

    const listResponse = await client.send(listCommand);
    console.log(`Found ${listResponse.StackSummaries?.length || 0} stacks to evaluate`);

    for (const stackSummary of listResponse.StackSummaries || []) {
      const stackName = stackSummary.StackName;
      const lastModifiedTime = stackSummary.LastUpdatedTime || stackSummary.CreationTime;

      if (lastModifiedTime.getTime() > cutoffTime) {
        continue;
      }

      try {
        const describeCommand = new DescribeStacksCommand({
          StackName: stackName
        });

        const describeResponse = await client.send(describeCommand);
        const stack = describeResponse.Stacks?.[0];

        if (!stack) {
          continue;
        }

        const hasIntegrationTestTag = stack.Tags?.some(
          tag => tag.Key === 'extension_integration_test' && tag.Value === 'true'
        );

        if (hasIntegrationTestTag) {
          stacksToDelete.push(stackName);
        } else {
          console.log(`Skipping ${stackName} - does not have extension_integration_test tag`);
        }
      } catch (error) {
        console.error(`Error describing stack ${stackName}:`, error.message);
      }
    }

    console.log(`Found ${stacksToDelete.length} stacks to delete`);
    const deletionResults = [];

    for (const stackName of stacksToDelete) {
      try {
        console.log(`Deleting stack: ${stackName}`);
        const deleteCommand = new DeleteStackCommand({
          StackName: stackName
        });
        await client.send(deleteCommand);
        deletionResults.push({
          stackName,
          success: true
        });
        console.log(`Successfully initiated deletion of ${stackName}`);
      } catch (error) {
        console.error(`Error deleting stack ${stackName}:`, error.message);
        deletionResults.push({
          stackName,
          success: false,
          errorMessage: error.message
        });
      }
    }

    console.log('Stack cleanup complete:', JSON.stringify(deletionResults, null, 2));

    return {
      statusCode: 200,
      body: JSON.stringify(deletionResults)
    };
  } catch (error) {
    console.error('Error during stack cleanup:', error);
    return {
      statusCode: 500,
      body: JSON.stringify({
        error: error.message
      })
    };
  }
};
