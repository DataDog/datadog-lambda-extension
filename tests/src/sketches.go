package main

import (
	"context"
	"fmt"
	b64 "encoding/base64"

	"github.com/golang/protobuf/proto"
	"github.com/DataDog/agent-payload/gogen"
	"github.com/aws/aws-lambda-go/events"
	"github.com/aws/aws-lambda-go/lambda"
)

func handleRequest(ctx context.Context, ev events.APIGatewayProxyRequest) (events.APIGatewayProxyResponse, error) {
	// payload is in base64 (to prevent API Gateway to try transforming the binary payload in utf8
	// transforming the payload in utf8 would result of adding padding 
	// + wrong char which would prevent protobuf unmarshalling
	payload, _ := b64.StdEncoding.DecodeString(ev.Body)
    sketchPayload := &gogen.SketchPayload{}
    err := proto.Unmarshal(payload, sketchPayload)
    if err != nil {
		fmt.Println("Unmarshaling error: ", err)
    } else {
		// output the paylaod to be able to retrieve it via cloudwatch
		fmt.Println(sketchPayload)
    }
	return events.APIGatewayProxyResponse{
		StatusCode: 200,
	}, nil
}

func main() {
	lambda.Start(handleRequest)
}

