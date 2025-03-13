stages:
  - test
  - compile
  - build
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
    - cd bottlecap && cargo fmt

cargo check:
  stage: test
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  needs: []
  script:
    - cd bottlecap && cargo check

cargo clippy:
  stage: test
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  needs: []
  script:
    - cd bottlecap && cargo clippy --all-features

{{ range $flavor := (ds "flavors").flavors }}

go agent ({{ $flavor.name }}):
  stage: compile
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
    FILE_SUFFIX: {{ $flavor.suffix }}
    FIPS: {{ $flavor.fips }}
  script:
    - echo "Building go agent based on $AGENT_BRANCH"
    # TODO: do this clone once in a separate job so that we can make sure that
    # we're using the same exact code for all of the builds (main can move
    # between different runs of the various compile jobs, for example)
    - cd .. && git clone -b $AGENT_BRANCH --single-branch https://github.com/DataDog/datadog-agent.git && cd datadog-agent && git rev-parse HEAD && cd ../datadog-lambda-extension
    - .gitlab/scripts/compile_go_agent.sh

bottlecap ({{ $flavor.name }}):
  stage: compile
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
    FIPS: {{ $flavor.fips }}
    FILE_SUFFIX: {{ $flavor.suffix }}
  script:
    - .gitlab/scripts/compile_bottlecap.sh

layer ({{ $flavor.name }}):
  stage: build
  image: registry.ddbuild.io/images/docker:20.10
  tags: ["arch:amd64"]
  needs:
    - go agent ({{ $flavor.name }})
    - bottlecap ({{ $flavor.name }})
    - cargo fmt
    - cargo check
    - cargo clippy
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
    - .gitlab/scripts/check_layer_size.s

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
    expire_in: 1 day # Signed layers should expire after 1 day
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
    - .gitlab/scripts/publish_layer.sh

{{ end }} # end environments

publish layer sandbox [us-east-1] ({{ $flavor.name }}):
  stage: self-monitoring
  tags: ["arch:amd64"]
  image: ${CI_DOCKER_TARGET_IMAGE}:${CI_DOCKER_TARGET_VERSION}
  rules:
    - when: manual
      allow_failure: true

  needs:
    - layer ({{ $flavor.name }})

  dependencies:
    - layer ({{ $flavor.name }})

  {{ with $environment := (ds "environments").environments.sandbox }}
  variables:
    LAYER_NAME_BASE_SUFFIX: {{ $flavor.layer_name_base_suffix }}
    REGION: us-east-1
    ARCHITECTURE: {{ $flavor.arch }}
    LAYER_FILE: datadog_extension-{{ $flavor.suffix }}.zip
    ADD_LAYER_VERSION_PERMISSIONS: {{ $environment.add_layer_version_permissions }}
    AUTOMATICALLY_BUMP_VERSION: {{ $environment.automatically_bump_version }}
  before_script:
    - EXTERNAL_ID_NAME={{ $environment.external_id }} ROLE_TO_ASSUME={{ $environment.role_to_assume }} AWS_ACCOUNT={{ $environment.account }} source .gitlab/scripts/get_secrets.sh
  {{ end }}
  script:
    - .gitlab/scripts/publish_layer.sh

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
    ALPINE: {{ $multi_arch_image_flavor.alpine }}
    SUFFIX: {{ $multi_arch_image_flavor.suffix }}
    PLATFORM: {{ $multi_arch_image_flavor.platform }}
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
    ALPINE: {{ $multi_arch_image_flavor.alpine }}
    SUFFIX: {{ $multi_arch_image_flavor.suffix }}
    PLATFORM: {{ $multi_arch_image_flavor.platform }}
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
    {{ if .needs_layer_publish }}
    - layer ({{ .name }})
    {{ end }} # end needs_layer_publish
    {{ end }} # end flavors
  dependencies:
    {{ range (ds "flavors").flavors }}
    {{ if .needs_layer_publish }}
    - layer ({{ .name }})
    {{ end }} # end needs_layer_publish
    {{ end }} # end flavors
  artifacts:
    expire_in: 1 hr
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
    {{ range (ds "flavors").flavors }}
    {{ if .needs_layer_publish }}
    - sign layer ({{ .name }})
    {{ end }} # end needs_layer_publish
    {{ end }} # end flavors
  dependencies:
    {{ range (ds "flavors").flavors }}
    {{ if .needs_layer_publish }}
    - sign layer ({{ .name }})
    {{ end }} # end needs_layer_publish
    {{ end }} # end flavors
  artifacts:
    expire_in: 1 day
    paths:
      - datadog_extension-signed-bundle-${CI_JOB_ID}/
    name: datadog_extension-signed-bundle-${CI_JOB_ID}
  script:
    - rm -rf datadog_extension-signed-bundle-${CI_JOB_ID}
    - mkdir -p datadog_extension-signed-bundle-${CI_JOB_ID}
    - cp .layers/datadog_extension-*.zip datadog_extension-signed-bundle-${CI_JOB_ID}
