.PHONY: build install test
build:            ## ./devsweep
	go build -o devsweep .
install:          ## into $(go env GOBIN), ~/go/bin here
	go install .
test:
	go vet ./... && go test ./...
