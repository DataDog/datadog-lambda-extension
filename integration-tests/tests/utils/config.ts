/**
 * Test configuration utilities
 */

import * as os from 'os';

/**
 * Get the suffix for resource naming
 * Priority:
 * 1. IDENTIFIER environment variable
 * 2. First name from username (e.g., 'john' from 'john.doe')
 * 3. Default: 'integration'
 */
function getIdentifier(): string {
  if (process.env.IDENTIFIER) {
    return process.env.IDENTIFIER;
  }
  
  try {
    // Get username (equivalent to whoami)
    const username = os.userInfo().username;
    // Parse first.last format to get first name
    const firstName = username.split('.')[0];
    if (firstName && firstName.length > 0) {
      return firstName;
    }
  } catch (error) {
    // Fall through to default
  }
  
  return 'integration';
}

/**
 * Get a full function name with the configured identifier
 * @param name - The base name of the function (e.g., 'exampleTestFunction')
 * @returns The full function name with identifier (e.g., 'exampleTestFunction-integration')
 */
export function getFunctionName(name: string): string {
  const identifier = getIdentifier();
  return `${name}-${identifier}`;
}
