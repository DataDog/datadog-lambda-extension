package main

import (
	"context"
	"fmt"

	ddlambda "github.com/DataDog/datadog-lambda-go"
	"github.com/aws/aws-lambda-go/lambda"
)

type MyEvent struct {
	Name string `json:"name"`
}

func PingHandler(ctx context.Context, name MyEvent) (string, error) {
	ddlambda.Metric("go.test.metric", 10.0, "test:extension-go")
	return fmt.Sprintf("Go Pong"), nil
}

func main() {
	lambda.Start(ddlambda.WrapHandler(PingHandler, nil))
}
