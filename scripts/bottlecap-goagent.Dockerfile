# syntax = docker/dockerfile:experimental
# zip the extension
FROM ubuntu:latest as compresser
ARG CMD_PATH
ARG DATADOG_WRAPPER=datadog_wrapper

RUN apt-get update
RUN apt-get install -y zip binutils
RUN mkdir /extensions
WORKDIR /extensions
COPY --from=public.ecr.aws/datadog/lambda-extension:57 /opt/extensions/datadog-agent /extensions/datadog-agent
RUN strip /extensions/datadog-agent

COPY ./scripts/$DATADOG_WRAPPER /$DATADOG_WRAPPER
RUN chmod +x /$DATADOG_WRAPPER
RUN zip -r datadog_extension.zip /extensions /$DATADOG_WRAPPER

# keep the smallest possible docker image
FROM scratch
COPY --from=compresser /extensions/datadog_extension.zip /
ENTRYPOINT ["/datadog_extension.zip"]
