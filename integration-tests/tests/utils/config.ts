import * as os from 'os';

export function getIdentifier(): string {
  if (process.env.IDENTIFIER) {
    return process.env.IDENTIFIER;
  }

  try {
    const username = os.userInfo().username;
    const firstName = username.split('.')[0];
    if (firstName && firstName.length > 0) {
      return firstName;
    }
  } catch (error) {
    console.error('Error getting identifier:', error);
  }

  return 'integration';
}
