stages:
  # TODO: swap these back once we're happy with the compile speed
  - compile binaries
  - check code
  - build
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

{{ range $flavor := (ds "flavors").flavors }}

{{ if $flavor.needs_code_checks }}

cargo fmt ({{ $flavor.arch }}):
  stage: check code
  tags: ["arch:{{ $flavor.arch }}"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  needs: []
  script:
    - cd bottlecap && cargo fmt

cargo check ({{ $flavor.arch }}):
  stage: check code
  tags: ["arch:{{ $flavor.arch }}"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  needs: []
  script:
    - cd bottlecap && cargo check

cargo clippy ({{ $flavor.arch }}):
  stage: check code
  tags: ["arch:{{ $flavor.arch }}"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  needs: []
  script:
    - cd bottlecap && cargo clippy --all-features

{{ end }} # end needs_code_checks

go agent ({{ $flavor.name }}):
  stage: compile binaries
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  needs: []
  artifacts:
    expire_in: 1 hr
    paths:
      - .binaries/datadog-agent-{{ $flavor.suffix }}
  variables:
    ARCHITECTURE: {{ $flavor.arch }}
    ALPINE: {{ $flavor.alpine }}
    SUFFIX: {{ $flavor.suffix }}
  script:
    - echo "Building go agent based on $AGENT_BRANCH"
    # TODO: do this clone once in a separate job so that we can make sure that
    # we're using the same exact code for all of the builds (main can move
    # between different runs of the various compile jobs, for example)
    - cd .. && git clone -b $AGENT_BRANCH --single-branch https://github.com/DataDog/datadog-agent.git && cd datadog-agent && git rev-parse HEAD && cd ../datadog-lambda-extension
    - .gitlab/scripts/compile_go_agent.sh

bottlecap ({{ $flavor.name }}):
  stage: compile binaries
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  needs: []
  artifacts:
    expire_in: 1 hr
    paths:
      - .binaries/bottlecap-{{ $flavor.suffix }}
  variables:
    ARCHITECTURE: {{ $flavor.arch }}
    ALPINE: {{ $flavor.alpine }}
    SUFFIX: {{ $flavor.suffix }}
  script:
    - .gitlab/scripts/compile_bottlecap.sh

layer ({{ $flavor.name }}):
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  needs:
    - go agent ({{ $flavor.name }})
    - bottlecap ({{ $flavor.name }})
    - cargo fmt ({{ $flavor.arch }})
    - cargo check ({{ $flavor.arch }})
    - cargo clippy ({{ $flavor.arch }})
  dependencies:
    - go agent ({{ $flavor.name }})
    - bottlecap ({{ $flavor.name }})
  artifacts:
    expire_in: 1 hr
    paths:
      - .layers/datadog_extension-{{ $flavor.suffix }}.zip
      - .layers/datadog_extension-{{ $flavor.suffix }}/*
  variables:
    ARCHITECTURE: {{ $flavor.arch }}
    SUFFIX: {{ $flavor.suffix }}
  script:
    - .gitlab/scripts/build_layer.sh

{{ if $flavor.needs_layer_publish }}

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
  script:
    - .gitlab/scripts/check_layer_size.sh

{{ range $environment := (ds "environments").environments }}

publish layer {{ $environment.name }} ({{ $flavor.name }}):
  stage: publish
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  rules:
    - if: '"{{ $environment.name }}" =~ /^(sandbox|staging)/'
      when: manual
      allow_failure: true
    - if: '$CI_COMMIT_TAG =~ /^v.*/'

  needs:
    - check layer size ({{ $flavor.name }})
{{ if or (eq $environment.name "prod") }}
    - sign layer ({{ $flavor.name }})
{{ else }}
    - layer ({{ $flavor.name }})
{{ end }} #end if prod

  dependencies:
{{ if or (eq $environment.name "prod") }}
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
    ARCHITECTURE: {{ $flavor.arch }}
    LAYER_FILE: datadog_extension-{{ $flavor.suffix }}.zip
    STAGE: {{ $environment.name }}
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
  script:
    - .gitlab/scripts/publish_layers.sh

{{ if eq $environment.name "sandbox" }}

publish self-monitoring sandbox layer ({{ $flavor.name }}):
  stage: publish
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  rules:
    - when: manual
      allow_failure: true

  needs:
    - check layer size ({{ $flavor.name }})
    - layer ({{ $flavor.name }})

  dependencies:
    - layer ({{ $flavor.name }})

  variables:
    REGION: us-east-1
    ARCHITECTURE: {{ $flavor.arch }}
    LAYER_FILE: datadog_extension-{{ $flavor.suffix }}.zip
    STAGE: {{ $environment.name }}
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
  script:
    - .gitlab/scripts/publish_layers.sh

{{ end }} # if environment sandbox

{{ end }} # end environments

{{ end }} # end needs_layer_publish

{{ end }}  # end flavors

{{ range $architecture := (ds "architectures").architectures }}

{{ range $environment := (ds "environments").environments }}

{{ if or (eq $environment.name "prod") }}
sign layer ({{ $architecture.name }}):
  stage: sign
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  rules:
    - if: '$CI_COMMIT_TAG =~ /^v.*/'
      when: manual
  needs:
    - layer ({{ $architecture.name }})
    - check layer size ({{ $architecture.name }})
  dependencies:
    - layer ({{ $architecture.name }})
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

{{ if or (eq $environment.name "sandbox") }}
publish private images:
  stage: publish
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  needs:
    - layer (arm64)
    - layer (amd64)
  when: manual
  dependencies:
    - layer (arm64)
    - layer (amd64)
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
  script:
    - .gitlab/scripts/build_private_image.sh  

publish private images (alpine):
  stage: publish
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  needs:
    - layer (arm64, alpine)
    - layer (amd64, alpine)
  when: manual
  dependencies:
    - layer (arm64, alpine)
    - layer (amd64, alpine)
  variables:
    ALPINE: 1
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
  script:
    - .gitlab/scripts/build_private_image.sh  
{{ end }}

{{- end }} # environments end

{{- end }} # architectures end

build images:
  stage: build
  tags: ["arch:amd64"]
  image: registry.ddbuild.io/images/docker:20.10
  rules:
    - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
    - layer (arm64)
    - layer (amd64)
  dependencies:
    - layer (arm64)
    - layer (amd64)
  script:
    - .gitlab/scripts/build_image.sh

build images (alpine):
  stage: build
  tags: ["arch:amd64"]
  image: registry.ddbuild.io/images/docker:20.10
  rules:
    - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
    - layer (arm64, alpine)
    - layer (amd64, alpine)
  dependencies:
    - layer (arm64, alpine)
    - layer (amd64, alpine)
  variables:
    ALPINE: 1
  script:
    - .gitlab/scripts/build_image.sh

publish images:
  stage: publish
  rules:
    - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
    - build images
  when: manual
  trigger:
    project: DataDog/public-images
    branch: main
    strategy: depend
  variables:
    IMG_SOURCES: ${CI_DOCKER_TARGET_IMAGE}:v${CI_PIPELINE_ID}-${CI_COMMIT_SHORT_SHA}
    IMG_DESTINATIONS: lambda-extension:${VERSION},lambda-extension:latest
    IMG_REGISTRIES: dockerhub,ecr-public,gcr-datadoghq
    IMG_SIGNING: false

publish images (alpine):
  stage: publish
  rules:
    - if: '$CI_COMMIT_TAG =~ /^v.*/'
  needs:
    - build images (alpine)
  when: manual
  trigger:
    project: DataDog/public-images
    branch: main
    strategy: depend
  variables:
    IMG_SOURCES: ${CI_DOCKER_TARGET_IMAGE}:v${CI_PIPELINE_ID}-${CI_COMMIT_SHORT_SHA}-alpine
    IMG_DESTINATIONS: lambda-extension:${VERSION}-alpine,lambda-extension:latest-alpine
    IMG_REGISTRIES: dockerhub,ecr-public,gcr-datadoghq
    IMG_SIGNING: false

