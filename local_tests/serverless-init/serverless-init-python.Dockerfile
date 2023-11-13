FROM debian:11-slim AS build
RUN apt-get update && \
    apt-get install --no-install-suggests --no-install-recommends --yes python3-venv gcc libpython3-dev && \
    python3 -m venv /venv && \
    /venv/bin/pip install --upgrade pip setuptools wheel

FROM build AS build-venv
COPY requirements.txt /requirements.txt
RUN /venv/bin/pip install --disable-pip-version-check -r /requirements.txt

FROM gcr.io/distroless/python3-debian11

COPY --from=build-venv /venv /venv

COPY datadog-agent /app/datadog-init

COPY app.py /app/app.py

WORKDIR /app

ENV PYTHONUNBUFFERED=1
ENV DD_VERSION=1

ENV DD_API_KEY=NO_NEED_TO_BE_VALID
ENV DD_CAPTURE_LAMBDA_PAYLOAD=false
ENV DD_ENV=dev
ENV DD_LAMBDA_HANDLER=func.handler
ENV DD_LOGS_INJECTION=false
ENV DD_LOG_LEVEL=DEBUG
ENV DD_MERGE_XRAY_TRACES=false
ENV DD_SERVERLESS_LOGS_ENABLED=false
ENV DD_SERVICE=integration-test-service
ENV DD_SITE=datadoghq.com
ENV DD_TAGS=testing:true
ENV DD_TRACE_ENABLED=true

ENV DD_APM_DD_URL=http://127.0.0.1:3333
ENV DD_DD_URL=http://127.0.0.1:3333
ENV DD_LOGS_CONFIG_LOGS_DD_URL=127.0.0.1:3333
ENV DD_LOGS_CONFIG_LOGS_NO_SSL=true
ENV DD_LOGS_ENABLED=true
ENV DD_LOCAL_TEST=1

ENV DD_PROXY_HTTP=http://google2.com:80
ENV DD_PROXY_HTTPS=http://google2.com:80

ENTRYPOINT ["/app/datadog-init"]
CMD ["/venv/bin/ddtrace-run", "/venv/bin/python3", "app.py"]