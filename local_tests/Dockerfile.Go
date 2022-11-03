FROM golang AS builder
WORKDIR /app
COPY golang .
RUN go build -v -o app .

FROM public.ecr.aws/lambda/go:1

# Add Datadog library
COPY --from=builder /app/app ${LAMBDA_TASK_ROOT}

# Copy both the datadog extension and the recorder one
RUN mkdir -p /opt/extensions
#COPY recorder-extension /opt/extensions/
COPY datadog-agent /opt/extensions/

# Make sure that the extension will send the payload to the man in the middle
# (recorder extension is listenning on 3333)
ENV DD_UNIVERSAL_INSTRUMENTATION=true
ENV DD_API_KEY=538c103f578af492d6e141d79b3dc941
#ENV DD_APM_DD_URL=http://127.0.0.1:3333
#ENV DD_DD_URL=http://127.0.0.1:3333
#ENV DD_LOGS_CONFIG_LOGS_DD_URL=127.0.0.1:3333
#ENV DD_LOGS_CONFIG_LOGS_NO_SSL=true
ENV DD_LOGS_ENABLED=false
ENV DD_LOG_LEVEL=DEBUG
ENV DD_MERGE_XRAY_TRACES=false
ENV DD_SERVERLESS_LOGS_ENABLED=false
ENV DD_SERVICE=asm-serverless-test
ENV DD_ENV=julio
ENV DD_TRACE_ENABLED=true
ENV DD_LOCAL_TEST=1

CMD [ "app" ]
