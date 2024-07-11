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

build layer ({{ $architecture.name }}):
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  artifacts:
    expire_in: 1 hr
    paths:
      - .layers/datadog_bottlecap-{{ $architecture.name }}.zip
  variables:
    ARCHITECTURE: {{ $architecture.name }}
  script:
    - ./scripts/build_bottlecap_layer.sh

check layer size ({{ $architecture.name }}):
  stage: test
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  needs:
    - build layer ({{ $architecture.name }})
  dependencies:
    - build layer ({{ $architecture.name }})
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

publish layer {{ $environment.name }} ({{ $architecture.name }}):
  stage: publish
  tags: ["arch:amd64"]
  image: ${DOCKER_TARGET_IMAGE}:${DOCKER_TARGET_VERSION}
  rules:
    - if: '"{{ $environment.name }}" =~ /^(sandbox|staging)/'
      when: manual
      allow_failure: true
  needs:
    - build layer ({{ $architecture.name }})
    - check layer size ({{ $architecture.name }})
    - fmt ({{ $architecture.name }})
    - check ({{ $architecture.name }})
    - clippy ({{ $architecture.name }})
  dependencies:
    - build layer ({{ $architecture.name }})
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
