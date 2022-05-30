#!/bin/bash
args=("$@")

# lowercase DD_LOG_LEVEL
DD_LOG_LEVEL=$(echo "$DD_LOG_LEVEL" | tr '[:upper:]' '[:lower:]')

if [ "$DD_EXPERIMENTAL_ENABLE_PROXY" == "true" ]
then
  if [ "$DD_LOG_LEVEL" == "debug" ]
  then
    echo "[bootstrap] DD_EXPERIMENTAL_ENABLE_PROXY is true"
    echo "[bootstrap] original AWS_LAMBDA_RUNTIME_API value is $AWS_LAMBDA_RUNTIME_API"
  fi

  export AWS_LAMBDA_RUNTIME_API="127.0.0.1:9000"

  if [ "$DD_LOG_LEVEL" == "debug" ]
  then
    echo "[bootstrap] rerouting AWS_LAMBDA_RUNTIME_API to $AWS_LAMBDA_RUNTIME_API"
  fi
fi

# if it is .Net
echo "The runtime is $AWS_EXECUTION_ENV"
if [[ "$AWS_EXECUTION_ENV" == *"dotnet"* ]];
then
  echo "It's the .NET runtime!"
  export CORECLR_ENABLE_PROFILING="1"
  export CORECLR_PROFILER="{846F5F1C-F9AE-4B07-969E-05C26BC060D8}"
  export CORECLR_PROFILER_PATH="/opt/datadog/Datadog.Trace.ClrProfiler.Native.so"
  export DD_DOTNET_TRACER_HOME="/opt/datadog"
fi

# if it is java
if [[ "$AWS_EXECUTION_ENV" == *"java"* ]];
then
  echo "It's the Java runtime!"
  export DD_JMXFETCH_ENABLED="false"
  # DD_Agent_Jar=/opt/java/lib/dd-java-agent.jar
  DD_Agent_Jar=/opt/dd-java-agent-0.90.0-SNAPSHOT.jar
  if [ -f "$DD_Agent_Jar" ]; 
  then
    export JAVA_TOOL_OPTIONS="-javaagent:$DD_Agent_Jar -XX:+TieredCompilation -XX:TieredStopAtLevel=1"
  else
    echo "File $DD_Agent_Jar does not exist!"
  fi
fi

exec "${args[@]}"