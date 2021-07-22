package main

import (
	"context"
	"net/http"
	"time"

	"github.com/aws/aws-lambda-go/lambda"

	ddlambda "github.com/DataDog/datadog-lambda-go"
	"github.com/aws/aws-lambda-go/events"
	httptrace "gopkg.in/DataDog/dd-trace-go.v1/contrib/net/http"
	"gopkg.in/DataDog/dd-trace-go.v1/ddtrace/tracer"
)

type TestResponse struct {
	body       string `json:"body"`
	statusCode int    `json:"statusCode"`
}

func handleRequest(ctx context.Context, ev events.APIGatewayProxyRequest) (TestResponse, error) {

	req, _ := http.NewRequestWithContext(ctx, "GET", "https://www.datadoghq.com", nil)
	client := http.Client{}
	client = *httptrace.WrapClient(&client)
	client.Do(req)

	// Create a custom span
	s, _ := tracer.StartSpanFromContext(ctx, "child.span")
	time.Sleep(100 * time.Millisecond)
	s.Finish()

	resp := TestResponse{
		statusCode: 200,
		body:       "ok",
	}

	return resp, nil
}

func main() {
	lambda.Start(ddlambda.WrapHandler(handleRequest, nil))
}
