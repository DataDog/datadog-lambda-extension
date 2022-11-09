FROM public.ecr.aws/lambda/java:11 as builder
RUN yum install -y unzip
RUN curl -k -L -v -X GET https\://services.gradle.org/distributions/gradle-7.5.1-bin.zip > gradle-7.5.1-bin.zip
RUN unzip -d /opt/gradle gradle-7.5.1-bin.zip

WORKDIR /tmpBuild
COPY java/src ./src
COPY java/build.gradle .

RUN JAVA_HOME=/var/lang /opt/gradle/gradle-7.5.1/bin/gradle :buildZip
RUN unzip /tmpBuild/build/distributions/hello.zip -d /tmp

FROM public.ecr.aws/lambda/java:11

# Add Datadog library
COPY java/lib/dd-java-agent.jar /opt/dd-java-agent.jar

# Copy customer code
COPY --from=builder /tmp /var/task/

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
ENV JAVA_TOOL_OPTIONS="-javaagent:\"/opt/dd-java-agent.jar\" -XX:+TieredCompilation -XX:TieredStopAtLevel=1"
ENV DD_LOCAL_TEST=1
ENV URL_TO_FETCH=https://httpbin.org/get
ENV _HANDLER=com.serverless.Handler

CMD [ "com.serverless.Handler" ]