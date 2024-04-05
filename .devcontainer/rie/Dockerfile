FROM ubuntu AS builder

RUN apt-get update
RUN apt-get install -y curl

RUN mkdir -p ~/.aws-lambda-rie && \
    curl -Lo ~/.aws-lambda-rie/aws-lambda-rie \
      https://github.com/aws/aws-lambda-runtime-interface-emulator/releases/latest/download/aws-lambda-rie-x86_64 && \
    chmod +x ~/.aws-lambda-rie/aws-lambda-rie && \
    mv ~/.aws-lambda-rie/aws-lambda-rie /usr/local/bin/aws-lambda-rie

FROM gcr.io/distroless/static-debian12
COPY --from=builder /usr/local/bin/aws-lambda-rie /aws-lambda-rie
ENTRYPOINT [ "/aws-lambda-rie" ]