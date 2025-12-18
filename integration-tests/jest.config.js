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
      outputName: process.env.TEST_SUITE ? `junit-${process.env.TEST_SUITE}.xml` : 'junit.xml',
      classNameTemplate: '{classname}',
      titleTemplate: '{title}',
      ancestorSeparator: ' › ',
      usePathForSuiteName: true,
      suiteName: process.env.TEST_SUITE || 'all',
    }],
    ['jest-html-reporters', {
      publicPath: './test-results',
      filename: process.env.TEST_SUITE ? `test-report-${process.env.TEST_SUITE}.html` : 'test-report.html',
      pageTitle: `Datadog Lambda Extension Test Report${process.env.TEST_SUITE ? ` - ${process.env.TEST_SUITE}` : ''}`,
      expand: true,
    }],
  ],
};
