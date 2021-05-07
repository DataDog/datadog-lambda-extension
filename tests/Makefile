build:
	env GOOS=linux go build -ldflags="-s -w" -o bin/sketches src/sketches.go
	env GOOS=linux go build -ldflags="-s -w" -o bin/alwaysOK src/alwaysOK.go

clean:
	rm -rf ./bin