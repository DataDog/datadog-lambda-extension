FROM scratch
ARG TARGETARCH
COPY .layers/datadog_extension-$TARGETARCH/extensions/datadog-agent opt/extensions/datadog-agent
COPY --chmod=0755 scripts/datadog_wrapper opt/datadog_wrapper
