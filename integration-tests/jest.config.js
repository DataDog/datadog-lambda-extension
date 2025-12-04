module.exports = {
  preset: 'ts-jest',
  testEnvironment: 'node',
  roots: ['<rootDir>/tests'],
  testMatch: ['**/*.test.ts'],
  moduleFileExtensions: ['ts', 'tsx', 'js', 'jsx', 'json', 'node'],
  collectCoverageFrom: [
    'tests/**/*.ts',
    '!tests/**/*.test.ts',
    '!tests/**/*.d.ts',
  ],
  // Increase timeout for integration tests that involve Lambda invocations and waiting for Datadog
  testTimeout: 900000, // 15 minutes
  verbose: true,
  // Reporters for test results
  reporters: [
    'default', // Console output
    ['jest-junit', {
      outputDirectory: './test-results',
      outputName: 'junit.xml',
      classNameTemplate: '{classname}',
      titleTemplate: '{title}',
      ancestorSeparator: ' › ',
      usePathForSuiteName: true,
    }],
    ['jest-html-reporters', {
      publicPath: './test-results',
      filename: 'test-report.html',
      pageTitle: 'Datadog Lambda Extension Test Report',
      expand: true,
    }],
  ],
};
