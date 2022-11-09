FROM public.ecr.aws/lambda/java:11

# Add Datadog library
COPY java/dd-java-agent-0.90.0-SNAPSHOT.jar /opt/dd-java-agent-0.90.0-SNAPSHOT.jar

# Copy customer code
COPY java/out /var/task/

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
ENV DD_LOG_LEVEL=ERROR
ENV DD_MERGE_XRAY_TRACES=false
ENV DD_SERVERLESS_LOGS_ENABLED=false
ENV DD_SERVICE=integration-test-service
ENV DD_TRACE_ENABLED=true
ENV JAVA_TOOL_OPTIONS="-javaagent:\"/opt/dd-java-agent-0.90.0-SNAPSHOT.jar\" -XX:+TieredCompilation -XX:TieredStopAtLevel=1"
ENV DD_LOCAL_TEST=1
ENV DD_SHOULD_ERROR=""
ENV URL_TO_FETCH=https://httpbin.org/get
ENV _HANDLER=com.serverless.Handler
ENV DD_TRACE_SAMPLE_RATE=0.001

CMD [ "com.serverless.Handler" ]