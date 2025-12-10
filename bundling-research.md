# Lambda Function Bundling Research

## Current State Analysis

### 1. Java Functions (`base-java`, `otlp-java`)
**Bundling Approach:** Pre-build artifacts
- **CDK Stack:** Uses `lambda.Code.fromAsset('./lambda/base-java/target/function.jar')`
- **Build Process:** Maven compilation in Docker
- **Artifacts:** `target/function.jar`
- **Local Build:** `./lambda/base-java/build.sh`
- **CI Build:** Separate pipeline job with Docker-in-Docker
- **Status:** ✅ **Consistent** - works locally and in CI

### 2. .NET Functions (`base-dotnet`, `otlp-dotnet`)
**Bundling Approach:** Pre-build artifacts
- **CDK Stack:** Uses `lambda.Code.fromAsset('./lambda/base-dotnet/bin/function.zip')`
- **Build Process:** dotnet CLI in Docker
- **Artifacts:** `bin/function.zip`
- **Local Build:** `./lambda/base-dotnet/build.sh`
- **CI Build:** Separate pipeline job with Docker-in-Docker
- **Status:** ✅ **Consistent** - works locally and in CI

### 3. Python Functions
#### `base-python`
**Bundling Approach:** Direct directory reference (no build)
- **CDK Stack:** Uses `lambda.Code.fromAsset('./lambda/base-python')`
- **Dependencies:** None (uses Datadog layer)
- **Build Process:** None needed
- **Status:** ✅ Works in CI and locally

#### `otlp-python`
**Bundling Approach:** CDK automatic bundling at deploy time
- **CDK Stack:** Uses `lambda.Code.fromAsset('./lambda/otlp-python', { bundling: {...} })`
- **Dependencies:** `opentelemetry-api`, `opentelemetry-sdk`, `opentelemetry-exporter-otlp-proto-http`
- **Bundling:** Docker-based pip install during CDK synthesis
- **Build Process:** None - bundled during `cdk deploy`
- **Local Build:** ❌ No build script
- **CI Build:** ❌ Not pre-built, bundled during deploy
- **Status:** ❌ **FAILS IN CI** - requires Docker during deployment, but integration-deploy job doesn't have Docker access

### 4. Node.js Functions
#### `base-node`
**Bundling Approach:** Direct directory reference (no build)
- **CDK Stack:** Uses `lambda.Code.fromAsset('./lambda/base-node')`
- **Dependencies:** None (uses Datadog layer)
- **Build Process:** None needed
- **Status:** ✅ Works in CI and locally

#### `otlp-node`
**Bundling Approach:** Pre-installed dependencies (committed node_modules)
- **CDK Stack:** Uses `lambda.Code.fromAsset('./lambda/otlp-node')`
- **Dependencies:** OpenTelemetry packages in package.json
- **Build Process:** `npm install` already run, `node_modules/` committed to git
- **Local Build:** ❌ No build script (but can run `npm install`)
- **CI Build:** ❌ No pipeline job
- **Status:** ⚠️ **Inconsistent** - works but relies on committed node_modules

## Problems Identified

1. **`otlp-python` breaks CI**: Uses CDK bundling that requires Docker at deploy time, but the `integration-deploy` job doesn't have Docker access
2. **Inconsistent approaches**: Some functions pre-build, some use CDK bundling, some commit dependencies
3. **No local build scripts for Python/Node**: Java and .NET have `build.sh` scripts, but Python and Node don't
4. **Committed node_modules**: `otlp-node` has committed `node_modules/` which is generally an anti-pattern

## Solution Options

### Option 1: Pre-build Everything (Recommended)
**Approach:** Build all functions with dependencies ahead of time, consistent with Java/.NET pattern

**Pros:**
- ✅ Consistent approach across all runtimes
- ✅ Works in CI without Docker during deployment
- ✅ Faster deployments (no bundling during synthesis)
- ✅ Clear separation between build and deploy
- ✅ Works locally with build scripts
- ✅ No committed dependencies

**Cons:**
- ⚠️ Requires changes to Python/Node CDK stacks
- ⚠️ Need to create build scripts for Python/Node
- ⚠️ Need to add CI pipeline jobs for Python/Node

**Implementation:**
1. Create `build.sh` scripts for Python functions
2. Create `build.sh` scripts for Node.js functions
3. Add Python/Node build jobs to GitLab pipeline
4. Update CDK stacks to point to pre-built artifacts
5. Remove CDK bundling from `otlp-python` stack
6. Remove committed `node_modules` from `otlp-node`

### Option 2: Use CDK Bundling for Everything
**Approach:** Remove pre-build jobs, add CDK bundling to all stacks

**Pros:**
- ✅ Simpler pipeline (no separate build jobs)
- ✅ CDK handles all bundling automatically

**Cons:**
- ❌ Requires Docker during CDK deploy (need docker-in-docker in CI)
- ❌ Slower deployments (bundling during synthesis)
- ❌ Inconsistent with established Java/.NET pattern
- ❌ More complex local development (need Docker)
- ❌ Would require reworking existing Java/.NET stacks

### Option 3: Fix Docker Access in integration-deploy
**Approach:** Keep current mixed approach but give integration-deploy Docker access

**Pros:**
- ✅ Minimal changes to existing setup

**Cons:**
- ❌ Still inconsistent across runtimes
- ❌ Python/Node don't have local build scripts
- ❌ Committed node_modules remains
- ❌ Only fixes the immediate problem, doesn't improve consistency

## Recommended Solution: Option 1

Pre-build all Lambda functions consistently. This matches the established pattern for Java/.NET and provides the best developer experience.

### Implementation Steps:

1. **Create build scripts for Python functions:**
   - `integration-tests/lambda/otlp-python/build.sh`
   - Install dependencies to a directory that will be deployed

2. **Create build scripts for Node.js functions:**
   - `integration-tests/lambda/otlp-node/build.sh`
   - Run `npm install` to generate fresh `node_modules`

3. **Update datasource to include Python/Node functions:**
   - Add to `.gitlab/datasources/lambda-functions.yaml`

4. **Add CI pipeline jobs for Python/Node:**
   - Similar to Java/.NET build jobs
   - Use appropriate Docker images (Python, Node)

5. **Update CDK stacks:**
   - Remove CDK bundling from `otlp-python-stack.ts`
   - Point to pre-built artifact directories
   - Ensure all stacks use consistent `Code.fromAsset()` approach

6. **Clean up:**
   - Remove committed `node_modules` from `otlp-node`
   - Add to `.gitignore`
   - Update documentation

### Expected File Structure After Implementation:

```
integration-tests/lambda/
├── base-java/
│   ├── build.sh          ✅ Exists
│   └── target/function.jar (artifact)
├── otlp-java/
│   ├── build.sh          ✅ Exists
│   └── target/function.jar (artifact)
├── base-dotnet/
│   ├── build.sh          ✅ Exists
│   └── bin/function.zip (artifact)
├── otlp-dotnet/
│   ├── build.sh          ✅ Exists
│   └── bin/function.zip (artifact)
├── base-python/
│   └── lambda_function.py (no dependencies, no build needed)
├── otlp-python/
│   ├── build.sh          ❌ Need to create
│   ├── requirements.txt
│   └── build/            ❌ Will contain bundled code + dependencies
├── base-node/
│   └── index.js (no dependencies, no build needed)
└── otlp-node/
    ├── build.sh          ❌ Need to create
    ├── package.json
    └── build/            ❌ Will contain bundled code + node_modules
```
