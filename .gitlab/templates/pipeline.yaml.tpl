stages:
  - test
  - compile
  - build
  - integration-tests
  - self-monitoring
  - sign
  - publish

default:
  retry:
    max: 1
    when:
      - runner_system_failure

variables:
  CI_DOCKER_TARGET_IMAGE: registry.ddbuild.io/ci/datadog-lambda-extension
  CI_DOCKER_TARGET_VERSION: latest

cargo fmt:
  stage: test
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  needs: []
  script:
    - cd bottlecap && cargo fmt --all -- --check

cargo check:
  stage: test
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  needs: []
  script:
    - cd bottlecap && cargo check --workspace

cargo clippy:
  stage: test
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  needs: []
  script:
    - apt-get update && apt-get install -y --fix-missing --no-install-recommends golang-go
    - cd bottlecap
    # We need to do these separately because the fips feature is incompatible with the default feature.
    - cargo clippy --workspace --features default
    - cargo clippy --workspace --no-default-features --features fips

{{ range $flavor := (ds "flavors").flavors }}

bottlecap ({{ $flavor.name }}):
  stage: compile
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:{{ $flavor.arch }}"]
  needs: []
  # This job sometimes times out on GitLab for unclear reason.
  # Set a short timeout with retries to work around this.
  timeout: 10m
  retry:
    max: 2
    when:
      - stuck_or_timeout_failure
      - runner_system_failure
      - script_failure
  artifacts:
    expire_in: 1 week
    paths:
      - .binaries/bottlecap-{{ $flavor.suffix }}
  variables:
    ARCHITECTURE: {{ $flavor.arch }}
    ALPINE: {{ $flavor.alpine }}
    FIPS: {{ $flavor.fips }}
    FILE_SUFFIX: {{ $flavor.suffix }}
  script:
    - .gitlab/scripts/compile_bottlecap.sh

layer ({{ $flavor.name }}):
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:{{ $flavor.arch }}"]
  needs:
    - bottlecap ({{ $flavor.name }})
    - cargo fmt
    - cargo check
    - cargo clippy
  dependencies:
    - bottlecap ({{ $flavor.name }})
  artifacts:
    expire_in: 1 week
    paths:
      - .layers/datadog_extension-{{ $flavor.suffix }}.zip
      - .layers/datadog_extension-{{ $flavor.suffix }}/*
  variables:
    ARCHITECTURE: {{ $flavor.arch }}
    FILE_SUFFIX: {{ $flavor.suffix }}
  script:
    - .gitlab/scripts/build_layer.sh

{{ if and (index $flavor "max_layer_compressed_size_mb") (index $flavor "max_layer_uncompressed_size_mb") }}

check layer size ({{ $flavor.name }}):
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  needs:
    - layer ({{ $flavor.name }})
  dependencies:
    - layer ({{ $flavor.name }})
  variables:
    LAYER_FILE: datadog_extension-{{ $flavor.suffix }}.zip
    MAX_LAYER_COMPRESSED_SIZE_MB: {{ $flavor.max_layer_compressed_size_mb }}
    MAX_LAYER_UNCOMPRESSED_SIZE_MB: {{ $flavor.max_layer_uncompressed_size_mb }}
  script:
    - .gitlab/scripts/check_layer_size.sh

{{ end }} # end max_layer_compressed_size_mb

{{ if $flavor.needs_layer_publish }}
sign layer ({{ $flavor.name }}):
  stage: sign
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  rules:
    - if: '$CI_COMMIT_TAG =~ /^v.*/'
      when: manual
  needs:
    - layer ({{ $flavor.name }})
    - check layer size ({{ $flavor.name }})
  dependencies:
    - layer ({{ $flavor.name }})
  artifacts: # Re specify artifacts so the modified signed file is passed
    expire_in: 2 weeks # Signed layers should expire after 2 weeks
    paths:
      - .layers/datadog_extension-{{ $flavor.suffix }}.zip
  variables:
    LAYER_FILE: datadog_extension-{{ $flavor.suffix }}.zip
  before_script:
    {{ with $environment := (ds "environments").environments.prod }}
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
    {{ end }}
  script:
    - .gitlab/scripts/sign_layers.sh prod

{{ range $environment_name, $environment := (ds "environments").environments }}

publish layer {{ $environment_name }} ({{ $flavor.name }}):
  stage: publish
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  rules:
    - if: '"{{ $environment_name }}" == "sandbox"'
      when: manual
      allow_failure: true
    - if: '$CI_COMMIT_TAG =~ /^v.*/'

  needs:
{{ if eq $environment_name "prod" }}
    - check layer size ({{ $flavor.name }})
    - sign layer ({{ $flavor.name }})
{{ else }}
    - layer ({{ $flavor.name }})
{{ end }} #end if prod

  dependencies:
{{ if or (eq $environment_name "prod") }}
      - sign layer ({{ $flavor.name }})
{{ else }}
      - layer ({{ $flavor.name }})
{{ end }} #end if prod

  parallel:
    matrix:
      - REGION: {{ range (ds "regions").regions }}
          - {{ .code }}
        {{- end}}
  variables:
    LAYER_NAME_BASE_SUFFIX: {{ $flavor.layer_name_base_suffix }}
    ARCHITECTURE: {{ $flavor.arch }}
    LAYER_FILE: datadog_extension-{{ $flavor.suffix }}.zip
    ADD_LAYER_VERSION_PERMISSIONS: {{ $environment.add_layer_version_permissions }}
    AUTOMATICALLY_BUMP_VERSION: {{ $environment.automatically_bump_version }}
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
  script:
    - .gitlab/scripts/publish_layers.sh

{{ end }} # end environments

publish layer [self-monitoring] ({{ $flavor.name }}):
  stage: self-monitoring
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  rules:
    - when: manual
      allow_failure: true
  parallel:
    matrix:
      - REGION: us-east-1 # Self Monitoring
      - REGION: us-west-2 # E2E Testing
  needs:
    - layer ({{ $flavor.name }})
  dependencies:
    - layer ({{ $flavor.name }})
  {{ with $environment := (ds "environments").environments.sandbox }}
  variables:
    LAYER_NAME_BASE_SUFFIX: {{ $flavor.layer_name_base_suffix }}
    ARCHITECTURE: {{ $flavor.arch }}
    LAYER_FILE: datadog_extension-{{ $flavor.suffix }}.zip
    ADD_LAYER_VERSION_PERMISSIONS: {{ $environment.add_layer_version_permissions }}
    AUTOMATICALLY_BUMP_VERSION: {{ $environment.automatically_bump_version }}
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
  {{ end }}
  script:
    - .gitlab/scripts/publish_layers.sh

{{ end }} # end needs_layer_publish

{{ end }}  # end flavors

{{ range $multi_arch_image_flavor := (ds "flavors").multi_arch_image_flavors }}

publish private images ({{ $multi_arch_image_flavor.name }}):
  stage: self-monitoring
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  when: manual
  needs:
    {{ range $multi_arch_image_flavor.dependency_names }}
    - layer ({{ . }})
    {{ end }} # end dependency_names
  dependencies:
    {{ range $multi_arch_image_flavor.dependency_names }}
    - layer ({{ . }})
    {{ end }} # end dependency_names
  variables:
    SUFFIX: {{ $multi_arch_image_flavor.suffix }}
  before_script:
    {{ with $environment := (ds "environments").environments.sandbox }}
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
    {{ end }}
  script:
    - .gitlab/scripts/build_private_image.sh

image ({{ $multi_arch_image_flavor.name }}):
  stage: build
  tags: ["arch:amd64"]
  image: registry.ddbuild.io/images/docker:20.10
  rules:
    - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
    {{ range $multi_arch_image_flavor.dependency_names }}
    - layer ({{ . }})
    {{ end }} # end dependency_names
  dependencies:
    {{ range $multi_arch_image_flavor.dependency_names }}
    - layer ({{ . }})
    {{ end }} # end dependency_names
  variables:
    SUFFIX: {{ $multi_arch_image_flavor.suffix }}
  script:
    - .gitlab/scripts/build_image.sh

publish image ({{ $multi_arch_image_flavor.name }}):
  stage: publish
  rules:
    - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
    - image ({{ $multi_arch_image_flavor.name }})
  when: manual
  trigger:
    project: DataDog/public-images
    branch: main
    strategy: depend
  variables:
    IMG_SOURCES: ${CI_DOCKER_TARGET_IMAGE}:v${CI_PIPELINE_ID}-${CI_COMMIT_SHORT_SHA}{{ $multi_arch_image_flavor.suffix }}
    IMG_DESTINATIONS: lambda-extension:${VERSION}{{ $multi_arch_image_flavor.suffix }},lambda-extension:latest{{ $multi_arch_image_flavor.suffix }}
    IMG_REGISTRIES: public
    IMG_SIGNING: false

{{ end }} # end multi_arch_image_flavors

layer bundle:
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  needs:
    {{ range (ds "flavors").flavors }}
    - layer ({{ .name }})
    {{ end }} # end flavors
  dependencies:
    {{ range (ds "flavors").flavors }}
    - layer ({{ .name }})
    {{ end }} # end flavors
  artifacts:
    expire_in: 1 week
    paths:
      - datadog_extension-bundle-${CI_JOB_ID}/
    name: datadog_extension-bundle-${CI_JOB_ID}
  script:
    - rm -rf datadog_extension-bundle-${CI_JOB_ID}
    - mkdir -p datadog_extension-bundle-${CI_JOB_ID}
    - cp .layers/datadog_extension-*.zip datadog_extension-bundle-${CI_JOB_ID}

signed layer bundle:
  stage: sign
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  rules:
    - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
    {{ range (ds "flavors").flavors }}{{ if .needs_layer_publish }}
    - sign layer ({{ .name }})
    {{ end }}{{ end }} # end flavors if needs_layer_publish
  dependencies:
    {{ range (ds "flavors").flavors }}{{ if .needs_layer_publish }}
    - sign layer ({{ .name }})
    {{ end }}{{ end }} # end flavors if needs_layer_publish
  artifacts:
    expire_in: 2 weeks
    paths:
      - datadog_extension-signed-bundle-${CI_JOB_ID}/
    name: datadog_extension-signed-bundle-${CI_JOB_ID}
  script:
    - rm -rf datadog_extension-signed-bundle-${CI_JOB_ID}
    - mkdir -p datadog_extension-signed-bundle-${CI_JOB_ID}
    - cp .layers/datadog_extension-*.zip datadog_extension-signed-bundle-${CI_JOB_ID}

# Integration Tests - Build Lambda functions in parallel by runtime

build java lambdas:
  stage: integration-tests
  image: registry.ddbuild.io/images/docker:27.3.1
  tags: ["docker-in-docker:arm64"]
  rules:
    - when: on_success
  needs: []
  cache:
    key: maven-cache-${CI_COMMIT_REF_SLUG}
    paths:
      - integration-tests/.cache/maven/
  artifacts:
    expire_in: 1 hour
    paths:
      - integration-tests/lambda/*/target/
  script:
    - cd integration-tests
    - ./scripts/build-java.sh

build dotnet lambdas:
  stage: integration-tests
  image: registry.ddbuild.io/images/docker:27.3.1
  tags: ["docker-in-docker:arm64"]
  rules:
    - when: on_success
  needs: []
  cache:
    key: nuget-cache-${CI_COMMIT_REF_SLUG}
    paths:
      - integration-tests/.cache/nuget/
  artifacts:
    expire_in: 1 hour
    paths:
      - integration-tests/lambda/*/bin/
  script:
    - cd integration-tests
    - ./scripts/build-dotnet.sh

build python lambdas:
  stage: integration-tests
  image: registry.ddbuild.io/images/docker:27.3.1
  tags: ["docker-in-docker:arm64"]
  rules:
    - when: on_success
  needs: []
  cache:
    key: pip-cache-${CI_COMMIT_REF_SLUG}
    paths:
      - integration-tests/.cache/pip/
  artifacts:
    expire_in: 1 hour
    paths:
      - integration-tests/lambda/*/package/
  script:
    - cd integration-tests
    - ./scripts/build-python.sh

build node lambdas:
  stage: integration-tests
  image: registry.ddbuild.io/images/docker:27.3.1
  tags: ["docker-in-docker:arm64"]
  rules:
    - when: on_success
  needs: []
  cache:
    key: npm-cache-${CI_COMMIT_REF_SLUG}
    paths:
      - integration-tests/.cache/npm/
  artifacts:
    expire_in: 1 hour
    paths:
      - integration-tests/lambda/*/node_modules/
  script:
    - cd integration-tests
    - ./scripts/build-node.sh

# Integration Tests - Publish arm64 layer with integration test prefix
publish integration layer (arm64):
  stage: integration-tests
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  rules:
    - when: on_success
  needs:
    - layer (arm64)
  dependencies:
    - layer (arm64)
  variables:
    LAYER_NAME_BASE_SUFFIX: ""
    ARCHITECTURE: arm64
    LAYER_FILE: datadog_extension-arm64.zip
    REGION: us-east-1
    ADD_LAYER_VERSION_PERMISSIONS: "0"
    AUTOMATICALLY_BUMP_VERSION: "1"
  {{ with $environment := (ds "environments").environments.sandbox }}
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
    - export PIPELINE_LAYER_SUFFIX="IntegTests-ARM"
    - export LAYER_DESCRIPTION="${CI_COMMIT_SHORT_SHA}"
    - echo "Publishing to layer Datadog-Extension-IntegTests-ARM with commit SHA ${LAYER_DESCRIPTION}"
  script:
    - .gitlab/scripts/publish_layers.sh
    - |
      LAYER_NAME="Datadog-Extension-IntegTests-ARM"
      COMMIT_SHA="${CI_COMMIT_SHORT_SHA}"
      LAYER_INFO=$(aws lambda list-layer-versions \
        --layer-name "${LAYER_NAME}" \
        --region us-east-1 \
        --query "LayerVersions[?Description=='${COMMIT_SHA}'].[Version, LayerVersionArn, Description] | [0]" \
        --output json)
      if [ "$LAYER_INFO" = "null" ] || [ -z "$LAYER_INFO" ]; then
        echo "ERROR: Could not find layer version with commit SHA ${COMMIT_SHA}"
        echo "Available versions:"
        aws lambda list-layer-versions --layer-name "${LAYER_NAME}" --region us-east-1 --query 'LayerVersions[*].[Version, Description]' --output table
        exit 1
      fi

      LAYER_VERSION=$(echo "$LAYER_INFO" | jq -r '.[0]')
      LAYER_ARN=$(echo "$LAYER_INFO" | jq -r '.[1]')
      LAYER_DESC=$(echo "$LAYER_INFO" | jq -r '.[2]')

      echo "Found layer version ${LAYER_VERSION} with ARN ${LAYER_ARN}"
      echo "Description: ${LAYER_DESC}"

      echo "EXTENSION_LAYER_ARN=${LAYER_ARN}" >> layer.env
      echo "LAYER_VERSION=${LAYER_VERSION}" >> layer.env
      echo "COMMIT_SHA=${COMMIT_SHA}" >> layer.env
  artifacts:
    reports:
      dotenv: layer.env
  {{ end }}

# Integration Tests - Deploy, test, and cleanup each suite independently (parallel by test suite)
integration-suite:
  stage: integration-tests
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  parallel:
    matrix:
      - TEST_SUITE: {{ range (ds "test_suites").test_suites }}
          - {{ .name }}
        {{- end}}
  rules:
    - when: on_success
  needs:
    - job: publish integration layer (arm64)
      artifacts: true
    - build java lambdas
    - build dotnet lambdas
    - build python lambdas
    - build node lambdas
  dependencies:
    - publish integration layer (arm64)
    - build java lambdas
    - build dotnet lambdas
    - build python lambdas
    - build node lambdas
  variables:
    IDENTIFIER: ${CI_COMMIT_SHORT_SHA}
    AWS_DEFAULT_REGION: us-east-1
    DD_SITE: datadoghq.com
  {{ with $environment := (ds "environments").environments.sandbox }}
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
    - curl -fsSL https://deb.nodesource.com/setup_20.x | bash -
    - apt-get install -y nodejs
    - cd integration-tests
    - npm ci
  {{ end }}
  script:
    - echo "Deploying ${TEST_SUITE} CDK stack with identifier ${IDENTIFIER}..."
    - echo "Using integration test layer - ${EXTENSION_LAYER_ARN} (version ${LAYER_VERSION}, commit ${COMMIT_SHA})"
    - |
      if [ -z "$EXTENSION_LAYER_ARN" ]; then
        echo "ERROR: EXTENSION_LAYER_ARN not set from publish job dotenv artifact"
        echo "This indicates the publish job failed to export the layer ARN"
        exit 1
      fi
    - export CDK_DEFAULT_ACCOUNT=$(aws sts get-caller-identity --query Account --output text)
    - export CDK_DEFAULT_REGION=us-east-1
    - npm run build
    - npx cdk deploy "integ-${IDENTIFIER}-${TEST_SUITE}" --require-approval never
    - echo "Running ${TEST_SUITE} integration tests with identifier ${IDENTIFIER}..."
    - export TEST_SUITE=${TEST_SUITE}
    - npx jest tests/${TEST_SUITE}.test.ts
  {{ with $environment := (ds "environments").environments.sandbox }}
  after_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
    - echo "Destroying ${TEST_SUITE} CDK stack with identifier ${IDENTIFIER}..."
    - |
      STACK_NAME="integ-${IDENTIFIER}-${TEST_SUITE}"

      # Check if stack exists
      STACK_STATUS=$(aws cloudformation describe-stacks \
        --stack-name "${STACK_NAME}" \
        --query 'Stacks[0].StackStatus' \
        --output text --region us-east-1 2>/dev/null || echo "DOES_NOT_EXIST")

      if [ "$STACK_STATUS" = "DOES_NOT_EXIST" ]; then
        echo "Stack ${STACK_NAME} does not exist, nothing to clean up"
      else
        echo "Found stack ${STACK_NAME} with status ${STACK_STATUS}"
        echo "Deleting stack ${STACK_NAME}..."
        aws cloudformation delete-stack --stack-name "${STACK_NAME}" --region us-east-1 || echo "Failed to delete ${STACK_NAME}, continuing..."

        echo "Waiting for stack deletion to complete..."
        aws cloudformation wait stack-delete-complete --stack-name "${STACK_NAME}" --region us-east-1 || echo "Stack ${STACK_NAME} deletion did not complete cleanly, continuing..."

        echo "${TEST_SUITE} stack deleted successfully"
      fi
  {{ end }}
  artifacts:
    when: always
    paths:
      - integration-tests/test-results/
    reports:
      junit: integration-tests/test-results/junit-*.xml
    expire_in: 30 days

# Integration Tests - Cleanup old layer versions
integration-cleanup-layer:
  stage: integration-tests
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  rules:
    - when: always
  needs:
    - job: integration-suite
  variables:
    LAYER_NAME: "Datadog-Extension-IntegTests-ARM"
    KEEP_VERSIONS: 500
  {{ with $environment := (ds "environments").environments.sandbox }}
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
  {{ end }}
  script:
    - echo "Cleaning up old versions of ${LAYER_NAME}..."
    - |
      # Get all layer versions sorted by version number (newest first)
      ALL_VERSIONS=$(aws lambda list-layer-versions \
        --layer-name "${LAYER_NAME}" \
        --query 'LayerVersions[*].Version' \
        --output text \
        --region us-east-1 2>/dev/null || echo "")

      if [ -z "$ALL_VERSIONS" ]; then
        echo "No versions found for layer ${LAYER_NAME}"
        exit 0
      fi

      VERSION_ARRAY=($ALL_VERSIONS)
      TOTAL_VERSIONS=${#VERSION_ARRAY[@]}

      echo "Found ${TOTAL_VERSIONS} versions of ${LAYER_NAME}"
      echo "Keeping the ${KEEP_VERSIONS} most recent versions"

      # Delete old versions (keep only the most recent KEEP_VERSIONS)
      if [ $TOTAL_VERSIONS -gt $KEEP_VERSIONS ]; then
        VERSIONS_TO_DELETE=${VERSION_ARRAY[@]:$KEEP_VERSIONS}

        for VERSION in $VERSIONS_TO_DELETE; do
          echo "Deleting ${LAYER_NAME} version ${VERSION}..."
          aws lambda delete-layer-version \
            --layer-name "${LAYER_NAME}" \
            --version-number "${VERSION}" \
            --region us-east-1 || echo "Failed to delete version ${VERSION}, continuing..."
        done

        echo "Successfully cleaned up old versions"
      else
        echo "Total versions (${TOTAL_VERSIONS}) <= keep versions (${KEEP_VERSIONS}), no cleanup needed"
      fi

