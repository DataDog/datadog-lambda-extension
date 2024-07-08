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
  GIT_DEPTH: 1

{{ range $architecture := (ds "architectures").architectures }}

build layer ({{ $architecture.name }}):
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  artifacts:
    expire_in: 1 hr
    paths:
      - .layers/datadog_extension-{{ $architecture.name }}.zip
  script:
    - cd .. && git clone -b $AGENT_BRANCH --single-branch https://github.com/DataDog/datadog-agent.git && cd datadog-lambda-extension
    - ARCHITECTURE={{ $architecture.name }} .gitlab/scripts/build_go_agent.sh

{{- end }} # architectures end