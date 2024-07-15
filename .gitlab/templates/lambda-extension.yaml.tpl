stages:
  - build
  - test
  - sign
  - publish

default:
  retry:
    max: 1
    when:
      - runner_system_failure

variables:
  DOCKER_TARGET_IMAGE: registry.ddbuild.io/ci/datadog-lambda-extension
  DOCKER_TARGET_VERSION: latest

{{ range $architecture := (ds "architectures").architectures }}

build go agent ({{ $architecture.name }}):
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  artifacts:
    expire_in: 1 hr
    paths:
      - .layers/datadog_extension-{{ $architecture.name }}.zip
      - .layers/datadog_extension-{{ $architecture.name }}/*
  variables:
    ARCHITECTURE: {{ $architecture.name }}
  script:
    - cd .. && git clone -b $AGENT_BRANCH --single-branch https://github.com/DataDog/datadog-agent.git && cd datadog-lambda-extension
    - .gitlab/scripts/build_go_agent.sh

build bottlecap ({{ $architecture.name }}):
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  needs:
    - build go agent ({{ $architecture.name }})
  dependencies:
    - build go agent ({{ $architecture.name }})
  artifacts:
    expire_in: 1 hr
    paths:
      - .layers/datadog_bottlecap-{{ $architecture.name }}.zip
  variables:
    ARCHITECTURE: {{ $architecture.name }}
  script:
    - .gitlab/scripts/build_bottlecap.sh

# Alpine
build go agent ({{ $architecture.name }}, alpine):
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  artifacts:
    expire_in: 1 hr
    paths:
      - .layers/datadog_extension-{{ $architecture.name }}-alpine.zip
      - .layers/datadog_extension-{{ $architecture.name }}-alpine/*
  variables:
    ARCHITECTURE: {{ $architecture.name }}
    ALPINE: 1
  script:
    - cd .. && git clone -b $AGENT_BRANCH --single-branch https://github.com/DataDog/datadog-agent.git && cd datadog-lambda-extension
    - .gitlab/scripts/build_go_agent.sh

build bottlecap ({{ $architecture.name }}, alpine):
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  needs:
    - build go agent ({{ $architecture.name }}, alpine)
  dependencies:
    - build go agent ({{ $architecture.name }}, alpine)
  artifacts:
    expire_in: 1 hr
    paths:
      - .layers/datadog_bottlecap-{{ $architecture.name }}-alpine.zip
      - .layers/datadog_bottlecap-{{ $architecture.name }}-alpine/*
  variables:
    ARCHITECTURE: {{ $architecture.name }}
    ALPINE: 1
  script:
    - .gitlab/scripts/build_bottlecap.sh

build image ({{ $architecture.name }}, alpine):
  stage: build
  tags: ["arch:amd64"]
  image: registry.ddbuild.io/images/docker:20.10
  when: manual
  needs:
    - build bottlecap ({{ $architecture.name }}, alpine)
  dependencies:
    - build bottlecap ({{ $architecture.name }}, alpine)
  variables:
    ARCHITECTURE: {{ $architecture.name }}
    ALPINE: 1
  script:
    - .gitlab/scripts/build_image.sh
# Alpine end

check layer size ({{ $architecture.name }}):
  stage: test
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  needs:
    - build bottlecap ({{ $architecture.name }})
  dependencies:
    - build bottlecap ({{ $architecture.name }})
  variables:
    LAYER_FILE: datadog_bottlecap-{{ $architecture.name }}.zip
  script:
    - .gitlab/scripts/check_layer_size.sh

fmt ({{ $architecture.name }}):
  stage: test
  tags: ["arch:{{ $architecture.name }}"]
  image: ${DOCKER_TARGET_IMAGE}:${DOCKER_TARGET_VERSION}
  needs: []
  script:
    - cd bottlecap && cargo fmt

check ({{ $architecture.name }}):
  stage: test
  tags: ["arch:{{ $architecture.name }}"]
  image: ${DOCKER_TARGET_IMAGE}:${DOCKER_TARGET_VERSION}
  needs: []
  script:
    - cd bottlecap && cargo check

clippy ({{ $architecture.name }}):
  stage: test
  tags: ["arch:{{ $architecture.name }}"]
  image: ${DOCKER_TARGET_IMAGE}:${DOCKER_TARGET_VERSION}
  needs: []
  script:
    - cd bottlecap && cargo clippy --all-features

{{ range $environment := (ds "environments").environments }}

{{ if or (eq $environment.name "prod") }}
sign layer ({{ $architecture.name }}):
  stage: sign
  tags: ["arch:amd64"]
  image: ${DOCKER_TARGET_IMAGE}:${DOCKER_TARGET_VERSION}
  rules:
    - if: '$CI_COMMIT_TAG =~ /^v.*/'
      when: manual
  needs:
    - build bottlecap ({{ $architecture.name }})
    - check layer size ({{ $architecture.name }})
    - fmt ({{ $architecture.name }})
    - check ({{ $architecture.name }})
    - clippy ({{ $architecture.name }})
  dependencies:
    - build bottlecap ({{ $architecture.name }})
  artifacts: # Re specify artifacts so the modified signed file is passed
    expire_in: 1 day # Signed layers should expire after 1 day
    paths:
      - .layers/datadog_bottlecap-{{ $architecture.name }}.zip
  variables:
    LAYER_FILE: datadog_bottlecap-{{ $architecture.name }}.zip
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
  script:
    - .gitlab/scripts/sign_layers.sh {{ $environment.name }}
{{ end }}

publish layer {{ $environment.name }} ({{ $architecture.name }}):
  stage: publish
  tags: ["arch:amd64"]
  image: ${DOCKER_TARGET_IMAGE}:${DOCKER_TARGET_VERSION}
  rules:
    - if: '"{{ $environment.name }}" =~ /^(sandbox|staging)/'
      when: manual
      allow_failure: true
    - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
{{ if or (eq $environment.name "prod") }}
      - sign layer ({{ $architecture.name }})
{{ else }}
      - build bottlecap ({{ $architecture.name }})
      - check layer size ({{ $architecture.name }})
      - fmt ({{ $architecture.name }})
      - check ({{ $architecture.name }})
      - clippy ({{ $architecture.name }})
{{ end }}
  dependencies:
{{ if or (eq $environment.name "prod") }}
      - sign layer ({{ $architecture.name }})
{{ else }}
      - build bottlecap ({{ $architecture.name }})
{{ end }}
  parallel:
    matrix:
      - REGION: {{ range (ds "regions").regions }}
          - {{ .code }}
        {{- end}}
  variables:
    ARCHITECTURE: {{ $architecture.name }}
    LAYER_FILE: datadog_bottlecap-{{ $architecture.name }}.zip
    STAGE: {{ $environment.name }}
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
  script:
    - .gitlab/scripts/publish_layers.sh

{{- end }} # environments end

{{- end }} # architectures end

{{ range $environment := (ds "environments").environments }}
  
# {{ if or (eq $environment.name "prod") }}
build images:
  stage: build
  tags: ["arch:amd64"]
  image: registry.ddbuild.io/images/docker:20.10
  # rules:
  #   - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
    - build bottlecap (arm64)
    - build bottlecap (amd64)
  dependencies:
    - build bottlecap (arm64)
    - build bottlecap (amd64)
  script:
    - .gitlab/scripts/build_image.sh

build images (alpine):
  stage: build
  tags: ["arch:amd64"]
  image: registry.ddbuild.io/images/docker:20.10
  # rules:
  #   - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
    - build bottlecap (arm64, alpine)
    - build bottlecap (amd64, alpine)
  dependencies:
    - build bottlecap (arm64, alpine)
    - build bottlecap (amd64, alpine)
  variables:
    ALPINE: 1
  script:
    - .gitlab/scripts/build_image.sh

publish images:
  stage: publish
  # rules:
  #   - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
    - build images
  when: manual
  # trigger:
  #   project: DataDog/public-images
  #   branch: main
  #   strategy: depend
  variables:
    IMG_SOURCES: ${DOCKER_TARGET_IMAGE}:v${CI_PIPELINE_ID}-${CI_COMMIT_SHORT_SHA}
    IMG_DESTINATIONS: datadog/lambda-extension:${VERSION},datadog/lambda-extension:latest
    IMG_REGISTRIES: dockerhub,ecr-public,gcr-datadoghq
  script:
    - echo "sources are ${IMG_SOURCES}"
    - echo "destinations are ${IMG_DESTINATIONS}"
    - echo "registries are ${IMG_REGISTRIES}"

publish images (alpine):
  stage: publish
  # rules:
  #   - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
    - build images (alpine)
  when: manual
  # trigger:
  #   project: DataDog/public-images
  #   branch: main
  #   strategy: depend
  variables:
    IMG_SOURCES: ${DOCKER_TARGET_IMAGE}:v${CI_PIPELINE_ID}-${CI_COMMIT_SHORT_SHA}-alpine
    IMG_DESTINATIONS: datadog/lambda-extension:${VERSION}-alpine,datadog/lambda-extension:latest-alpine
    IMG_REGISTRIES: dockerhub,ecr-public,gcr-datadoghq
  script:
    - echo "sources are ${IMG_SOURCES}"
    - echo "destinations are ${IMG_DESTINATIONS}"
    - echo "registries are ${IMG_REGISTRIES}"

# {{ end }}

{{- end }}
