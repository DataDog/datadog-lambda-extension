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

build layer (bottlecap) ({{ $architecture.name }}):
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  artifacts:
    expire_in: 1 hr
    paths:
      - .layers/datadog_bottlecap-{{ $architecture.name }}.zip
  script:
    - ARCHITECTURE={{ $architecture.name }} ./scripts/build_bottlecap_layer.sh

check layer size (bottlecap) ({{ $architecture.name }}):
  stage: test
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  needs:
    - build layer (bottlecap) ({{ $architecture.name }})
  dependencies:
    - build layer (bottlecap) ({{ $architecture.name }})
  script:
    - ARCHITECTURE={{ $architecture.name }} .gitlab/scripts/check_layer_size.sh

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

{{- end }} # architectures end