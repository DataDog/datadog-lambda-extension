package main

import (
	"context"
	"fmt"
	"log"

	"github.com/aws/aws-lambda-go/lambda"
)

type LogEvent struct {
	Name string `json:"name"`
}

func LogHandler(ctx context.Context, msg LogEvent) (string, error) {
	message := "this is a log message. beep boop"
	log.Printf(message)
	return fmt.Sprintf(message), nil
}

func main() {
	lambda.Start(LogHandler)
}
