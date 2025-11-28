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
    IMG_REGISTRIES: dockerhub,ecr-public,gcr-datadoghq
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
    - export PIPELINE_LAYER_SUFFIX="${CI_COMMIT_SHORT_SHA}"
    - echo "Publishing layer with suffix - ${PIPELINE_LAYER_SUFFIX}"
  script:
    - .gitlab/scripts/publish_layers.sh
    # Get the layer ARN we just published and save it as an artifact
    - LAYER_ARN=$(aws lambda list-layer-versions --layer-name "Datadog-Extension-${CI_COMMIT_SHORT_SHA}" --query 'LayerVersions[0].LayerVersionArn' --output text --region us-east-1)
    - echo "Published layer ARN - ${LAYER_ARN}"
    - echo "${LAYER_ARN}" > integration_layer_arn.txt
  artifacts:
    paths:
      - integration_layer_arn.txt
    expire_in: 1 hour
  {{ end }}

# Integration Tests - Deploy CDK stacks with commit hash prefix
integration-deploy:
  stage: integration-tests
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  rules:
    - when: on_success
  needs:
    - publish integration layer (arm64)
  dependencies:
    - publish integration layer (arm64)
  variables:
    IDENTIFIER: ${CI_COMMIT_SHORT_SHA}
    AWS_DEFAULT_REGION: us-east-1
  {{ with $environment := (ds "environments").environments.sandbox }}
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
    - curl -fsSL https://deb.nodesource.com/setup_20.x | bash -
    - apt-get install -y nodejs
    - cd integration-tests
    - npm ci
  {{ end }}
  script:
    - echo "Deploying CDK stacks with identifier ${IDENTIFIER}..."
    - export EXTENSION_LAYER_ARN=$(cat ../integration_layer_arn.txt)
    - echo "Using integration test layer - ${EXTENSION_LAYER_ARN}"
    - export CDK_DEFAULT_ACCOUNT=$(aws sts get-caller-identity --query Account --output text)
    - export CDK_DEFAULT_REGION=us-east-1
    - npm run build
    - npx cdk deploy "integ-$IDENTIFIER-*" --require-approval never

# Integration Tests - Run Jest test suite
integration-test:
  stage: integration-tests
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  rules:
    - when: on_success
  needs:
    - integration-deploy
  variables:
    IDENTIFIER: ${CI_COMMIT_SHORT_SHA}
    DD_SITE: datadoghq.com
  {{ with $environment := (ds "environments").environments.sandbox }}
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
    - curl -fsSL https://deb.nodesource.com/setup_20.x | bash -
    - apt-get install -y nodejs
    - cd integration-tests
    - npm ci
  script:
    - echo "Running integration tests with identifier ${IDENTIFIER}..."
    - npm run test:ci
  {{ end }}
  artifacts:
    when: always
    paths:
      - integration-tests/test-results/
    reports:
      junit: integration-tests/test-results/junit.xml
    expire_in: 30 days

# Integration Tests - Cleanup stacks
# integration-cleanup-stacks:
#   stage: integration-tests
#   tags: ["arch:amd64"]
#   image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
#   when: always
#   rules:
#     - when: always
#   needs:
#     - integration-test
#   variables:
#     IDENTIFIER: integration
#   {{ with $environment := (ds "environments").environments.sandbox }}
#   before_script:
#     - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
#     - curl -fsSL https://deb.nodesource.com/setup_20.x | bash -
#     - apt-get install -y nodejs
#     - cd integration-tests
#   {{ end }}
#   script:
#     - echo "Destroying CDK stacks with identifier ${IDENTIFIER}..."
#     - npx cdk destroy "IntegrationTests-$IDENTIFIER-*" --force || echo "Failed to destroy some stacks, but continuing..."

