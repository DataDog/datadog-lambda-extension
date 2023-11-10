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

COPY .container-opt/datadog-agent-go /app/datadog-init

COPY app.py /app/app.py

WORKDIR /app

ENV PYTHONUNBUFFERED=1
ENV DD_SERVICE=self-monitoring
ENV DD_VERSION=1
ENV DD_LOGS_ENABLED=true
ENV DD_API_KEY=DONT_CARE

ENTRYPOINT ["/app/datadog-init"]
CMD ["/venv/bin/ddtrace-run", "/venv/bin/python3", "app.py"]
