package main

import (
	"context"
	"fmt"

	"github.com/aws/aws-lambda-go/lambda"
)

type MyEvent struct {
	Name string `json:"name"`
}

func PingHandler(ctx context.Context, name MyEvent) (string, error) {
	return fmt.Sprintf("Go Pong"), nil
}

func main() {
	lambda.Start(PingHandler)
}
