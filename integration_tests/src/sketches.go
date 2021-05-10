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
      //log.Println("unmarshaling error: ", err)
    } else {
      //log.Println("Successfully created proto struct:", sketchPayload)
      for _, singleSketch := range sketchPayload.Sketches {
        fmt.Printf("[sketch] %#v\n", singleSketch)
      }
      fmt.Println("[metadata] ", sketchPayload.Metadata)
    }
	return events.APIGatewayProxyResponse{
		StatusCode: 200,
		Body:       "ok",
	}, nil
}

func main() {
	lambda.Start(handleRequest)
}

