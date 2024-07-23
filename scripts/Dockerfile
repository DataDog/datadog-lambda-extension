FROM scratch
ARG TARGETARCH
COPY .layers/datadog_bottlecap-$TARGETARCH/extensions/datadog-agent opt/extensions/datadog-agent
COPY .layers/datadog_bottlecap-$TARGETARCH/datadog-agent-go opt/datadog-agent-go
COPY --chmod=0755 scripts/datadog_wrapper opt/datadog_wrapper
