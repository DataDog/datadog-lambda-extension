CONTAINER_NAME=bottlecap-lambda
IMAGE?=$(CONTAINER_NAME):latest
PORT?=2345
# Set to number of seconds to wait for debugger at startup (default: 30s)
# Set DEBUG_WAIT=0 to start without wait (no debugger attachment needed)
DEBUG_WAIT?=30

# Get username once and normalize it
USERNAME_NORMALIZED=$(whoami | tr -d '.')

# Get current directory name for portability
CURRENT_DIR=$(shell basename $$(pwd))

help:
	@echo "Available targets:"
	@echo "  make build         - Build the bottlecap layer"
	@echo "  make start         - Start with 30s debug wait (default) for debugger attachment"
	@echo "  make start DEBUG_WAIT=0 - Start without wait (no debugger attachment needed)"
	@echo "  make start DEBUG_WAIT=15 - Start with custom debug wait (e.g., 15 seconds)"
	@echo "  make stop          - Stop the running Lambda container"
	@echo "  make status        - Check the status of the Lambda container"
	@echo "  make invoke        - Invoke the Lambda function"
	@echo "  make attach        - Attach gdbserver to the running extension"
	@echo "  make logs          - Follow the container logs"
	@echo "  make shell         - Open a shell session in the running container"


build:
	cd ../.. && \
	ARCHITECTURE=arm64 FIPS=0 ALPINE=0 DEBUG=1 scripts/build_bottlecap_layer.sh && \
	docker build -f local_tests/$(CURRENT_DIR)/Dockerfile --build-arg CURRENT_PARENT_DIR=$(CURRENT_DIR) -t $(IMAGE) . && \
	cd -

start:
	docker run -d --name $(CONTAINER_NAME) \
	-p 9000:8080 \
	-p 2345:2345 \
	--cap-add=SYS_PTRACE --security-opt seccomp=unconfined \
  	--cpus=1 \
  	-e DD_API_KEY="$(DD_API_KEY)" \
  	-e DD_SITE="datadoghq.com" \
  	-e DD_SERVICE="dd-$(USERNAME_NORMALIZED)-test" \
	-e DD_LOG_LEVEL="debug" \
  	-e DD_ENV="dev" \
  	-e DD_DEBUG_WAIT_FOR_ATTACH="$(DEBUG_WAIT)" \
  	--volume "$$(pwd)/entrypoint.sh:/entrypoint.sh" \
  	$(IMAGE)
	@echo "Container '$(CONTAINER_NAME)' started. Use 'make logs' to view logs."
	@if [ "$(DEBUG_WAIT)" != "0" ]; then \
		echo "Debug mode: Extension will wait $(DEBUG_WAIT) seconds for debugger on first invocation"; \
	fi

logs:
	docker logs -f $(CONTAINER_NAME)

stop:
	@docker stop $(CONTAINER_NAME) 2>/dev/null || true
	@docker rm $(CONTAINER_NAME) 2>/dev/null || true
	@echo "Container '$(CONTAINER_NAME)' stopped and removed."

status:
	@if docker ps --filter name=$(CONTAINER_NAME) --format '{{.Names}}' | grep -q $(CONTAINER_NAME); then \
		echo "up"; \
	else \
		echo "down"; \
	fi

# Helpful shell in a running container
shell:
	docker exec -it $(CONTAINER_NAME) bash

attach:
	@docker exec -it $(CONTAINER_NAME) sh -lc 'ps aux | grep datadog-agent | grep -v grep | awk "{print \$$2}" | xargs -I{} gdbserver --attach 0.0.0.0:$(PORT) {}'

# Trigger an invoke from the host against the RIE proxy on :9000
invoke:
	curl -XPOST "http://localhost:9000/2015-03-31/functions/function/invocations" -d '{}' --max-time 30