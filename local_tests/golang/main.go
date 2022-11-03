package main

import (
	"context"
	"fmt"

	ddlambda "github.com/DataDog/datadog-lambda-go"
	"github.com/aws/aws-lambda-go/events"
	"github.com/aws/aws-lambda-go/lambda"

	"gopkg.in/DataDog/dd-trace-go.v1/ddtrace/tracer"
)

func main() {
	// Wrap your lambda handler
	lambda.Start(ddlambda.WrapFunction(handleRequest, nil))
}

func handleRequest(ctx context.Context, event events.APIGatewayProxyRequest) (events.APIGatewayProxyResponse, error) {
	// Create a custom span
	s, _ := tracer.StartSpanFromContext(ctx, "child.span")
	defer s.Finish()

	fmt.Printf("Processing request data for request %s.\n", event.RequestContext.RequestID)
	fmt.Printf("Body size = %d.\n", len(event.Body))
	fmt.Println("Headers:")
	for key, value := range event.Headers {
		fmt.Printf("    %s: %s\n", key, value)
	}

	return events.APIGatewayProxyResponse{Body: event.Body, StatusCode: 200}, nil
}
