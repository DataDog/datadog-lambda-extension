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

func LogHandler(ctx context.Context, msg LogEvent) string {
	message := "this is a log message. beep boop"
	log.Printf(message)
	return fmt.Sprintf(message)
}

func main() {
	lambda.Start(LogHandler)
}
