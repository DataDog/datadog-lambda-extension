FROM public.ecr.aws/lambda/python:3.9

# Add Datadog library
COPY python /opt/python

# Copy customer code
COPY func.py /var/task/

# Copy both the datadog extension and the recorder one
RUN mkdir -p /opt/extensions
COPY recorder-extension /opt/extensions/
COPY datadog-agent /opt/extensions/

# Make sure that the extension will send the payload to the man in the middle
# (recorder extension is listenning on 3333)
ENV DD_API_KEY=NO_NEED_TO_BE_VALID
ENV DD_APM_DD_URL=http://127.0.0.1:3333
ENV DD_DD_URL=http://127.0.0.1:3333
ENV DD_LAMBDA_HANDLER=func.handler
ENV DD_LOGS_CONFIG_LOGS_DD_URL=127.0.0.1:3333
ENV DD_LOGS_CONFIG_LOGS_NO_SSL=true
ENV DD_LOGS_ENABLED=false
ENV DD_LOG_LEVEL=DEBUG
ENV DD_MERGE_XRAY_TRACES=false
ENV DD_SERVERLESS_LOGS_ENABLED=false
ENV DD_SERVICE=integration-test-service
ENV DD_TRACE_ENABLED=true
ENV DD_LOCAL_TEST=1

CMD [ "datadog_lambda.handler.handler" ]  