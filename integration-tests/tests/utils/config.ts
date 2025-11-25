/**
 * Test configuration utilities
 */

import * as os from 'os';

/**
 * Get the suffix for resource naming
 * Priority:
 * 1. SUFFIX environment variable
 * 2. First name from username (e.g., 'john' from 'john.doe')
 * 3. Default: 'local-testing'
 */
function getSuffix(): string {
  if (process.env.SUFFIX) {
    return process.env.SUFFIX;
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
  
  return 'local-testing';
}

/**
 * Get a full function name with the configured suffix
 * @param name - The base name of the function (e.g., 'exampleTestFunction')
 * @returns The full function name with suffix (e.g., 'exampleTestFunction-john')
 */
export function getFunctionName(name: string): string {
  const suffix = getSuffix();
  return `${name}-${suffix}`;
}
