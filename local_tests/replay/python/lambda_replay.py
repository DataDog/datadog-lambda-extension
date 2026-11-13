"""Use a lambda-replay recording as test fixtures.

Tests call the handler in-process (fast, debuggable); every outbound HTTP(S)
call it makes is answered from the recording by a `lambda-replay serve`
process. Tests assert on the handler's result as usual.

    # conftest.py: start before the handler module is imported, so SDK
    # clients created at import see the proxy and fake credentials.
    import lambda_replay
    replay = lambda_replay.start("tests/recordings")

    # test_orders.py
    import handler

    def test_stores_order():
        with replay.invocation(0) as inv:          # recorded invocation #0
            result = inv.invoke(handler.handler)
        assert result["qty"] == 2

    def test_new_case():                           # not in the recording yet
        with replay.invocation("bulk-order", event={"order_id": "o-9", "qty": 50}) as inv:
            result = inv.invoke(handler.handler)
        assert result["qty"] == 50

Run with `LAMBDA_REPLAY_UPDATE=1` to send calls the recording lacks to the
real upstream (with your real AWS credentials) and save them: a new test case
or a code change then costs only its new calls. Without it, an unrecorded call
fails the test with the closest recorded call and how it differs.

The binary is `$LAMBDA_REPLAY_BIN`, or `lambda-replay` on `PATH`.
"""

import atexit
import json
import os
import subprocess
import time
import urllib.request

__all__ = ["start", "Replay", "Invocation", "ReplayMismatch"]

# Variables the replay must control, whatever the environment already has.
_FORCED = {
    "HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "NO_PROXY", "no_proxy",
    "NODE_USE_ENV_PROXY", "AWS_CA_BUNDLE", "REQUESTS_CA_BUNDLE", "SSL_CERT_FILE",
    "NODE_EXTRA_CA_CERTS", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN",
}

_active = None


class ReplayMismatch(AssertionError):
    """The handler made calls the recording has no answer for."""


def start(recording, binary=None, update=None, rules=None):
    """Starts (once per process) a replay of `recording` and returns it."""
    global _active
    if _active is None:
        _active = Replay(recording, binary=binary, update=update, rules=rules)
    return _active


class Context:
    """The parts of the Lambda context object handlers commonly use."""

    def __init__(self, request_id, headers, env, timeout_s=30):
        self.aws_request_id = request_id
        self.function_name = env.get("AWS_LAMBDA_FUNCTION_NAME", "function")
        self.function_version = env.get("AWS_LAMBDA_FUNCTION_VERSION", "$LATEST")
        self.memory_limit_in_mb = env.get("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", "128")
        self.invoked_function_arn = (headers.get("lambda-runtime-invoked-function-arn") or [""])[0]
        self.log_group_name = env.get("AWS_LAMBDA_LOG_GROUP_NAME", "")
        self.log_stream_name = env.get("AWS_LAMBDA_LOG_STREAM_NAME", "")
        self._deadline = time.time() + timeout_s

    def get_remaining_time_in_millis(self):
        return max(0, int((self._deadline - time.time()) * 1000))


class Replay:
    def __init__(self, recording, binary=None, update=None, rules=None):
        if update is None:
            update = os.environ.get("LAMBDA_REPLAY_UPDATE", "").lower() in ("1", "true")
        binary = binary or os.environ.get("LAMBDA_REPLAY_BIN", "lambda-replay")
        args = [binary, "serve", "--recording", str(recording)]
        if update:
            args.append("--update")
        if rules:
            args += ["--rules", str(rules)]
        self._proc = subprocess.Popen(args, stdout=subprocess.PIPE, text=True)
        line = self._proc.stdout.readline()
        if not line:
            raise RuntimeError(f"lambda-replay serve exited: {self._proc.wait()}")
        info = json.loads(line)
        self.update = info["update"]
        self.invocations = info["invocations"]
        self._control = info["control"]
        self._env = info["env"]
        for k, v in self._env.items():
            if k in _FORCED:
                os.environ[k] = v
            else:
                os.environ.setdefault(k, v)
        # Control calls must not go through the proxy they configure.
        self._opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        self._stopped = False
        atexit.register(self.stop)

    def _post(self, path, body=None):
        req = urllib.request.Request(
            self._control + path,
            data=json.dumps(body or {}).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with self._opener.open(req, timeout=30) as r:
            return json.load(r)

    def invocation(self, key=0, event=None, strict=True):
        """A recorded invocation by index or request ID, or a new one named
        `key` with `event`. With `strict`, unrecorded calls fail on exit."""
        if isinstance(key, int):
            if key >= len(self.invocations):
                raise IndexError(f"the recording has {len(self.invocations)} invocations")
            request_id = self.invocations[key]["request_id"]
        else:
            request_id = key
        return Invocation(self, request_id, event, strict)

    def stop(self):
        """Stops the replay; with update on, saves the recording first."""
        if self._stopped:
            return
        self._stopped = True
        try:
            written = self._post("/shutdown").get("written")
            if written:
                print(f"lambda-replay: updated {written}")
        finally:
            try:
                self._proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self._proc.kill()


class Invocation:
    def __init__(self, replay, request_id, event, strict):
        self._replay = replay
        self.request_id = request_id
        self._given_event = event
        self.strict = strict
        self.event = None
        self.context = None
        self._result = None
        # After exit:
        self.served = 0
        self.unmatched = []
        self.forwarded = []
        self.missing = []

    def __enter__(self):
        body = {"request_id": self.request_id}
        if self._given_event is not None:
            body["event"] = self._given_event
        started = self._replay._post("/begin", body)
        self.event = started["event"]
        self.context = Context(self.request_id, started.get("headers", {}), os.environ)
        return self

    def invoke(self, handler):
        """Calls `handler(event, context)` and keeps the outcome for `--update`."""
        try:
            result = handler(self.event, self.context)
        except Exception as e:
            self._result = ("error", {"errorType": type(e).__name__, "errorMessage": str(e)})
            raise
        self._result = ("response", result)
        return result

    def __exit__(self, exc_type, exc, tb):
        body = {"request_id": self.request_id}
        if self._result:
            body["kind"], body["body"] = self._result
        report = self._replay._post("/end", body)
        self.served = report["served"]
        self.unmatched = report["unmatched"]
        self.forwarded = report["forwarded"]
        self.missing = report["missing"]
        if self.strict and self.unmatched and not self._replay.update:
            # Usually the handler already failed on the replay's error response;
            # explain why, keeping that failure as the cause.
            raise ReplayMismatch(self._describe()) from exc
        return False

    def _describe(self):
        lines = [f"{len(self.unmatched)} call(s) not in the recording for {self.request_id}:"]
        for u in self.unmatched:
            op = f" ({u['operation']})" if u.get("operation") else ""
            lines.append(f"  + {u['method']} {u['url']}{op}")
            near = u.get("near_miss")
            if near:
                lines.append(f"      closest recorded call is #{near['recorded_seq']}:")
                lines += [f"        {d}" for d in near["differences"]]
        lines.append("Run with LAMBDA_REPLAY_UPDATE=1 to record them.")
        return "\n".join(lines)
