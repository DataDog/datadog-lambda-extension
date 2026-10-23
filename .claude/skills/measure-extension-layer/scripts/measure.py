"""Force cold starts of test functions, one or more per variant, and save the CloudWatch log events.

Usage (through aws-vault, so boto3 gets credentials):
  aws-vault exec sso-serverless-sandbox-account-admin -- python3 measure.py \
      --out runs/<label> -n 20 noext=none old=v100 new=<layer-arn>

A variant is NAME=SPEC. SPEC is `none` (no extension), a released version (`v101`) or a layer
version ARN. Each round forces one cold start of each variant in turn, so drift over time
affects all variants the same way. With --copies K, each variant has K functions, and the
log events of all K go to the same <variant>.jsonl.
"""

import argparse
import io
import json
import os
import random
import re
import sys
import time
import zipfile
from concurrent.futures import ThreadPoolExecutor

import boto3
from botocore.exceptions import ConnectionError as BotoConnectionError

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from layer_size import REGION, layer_sizes, resolve  # noqa: E402

ACCOUNT = "425362996713"
# The secret exists only in us-east-2. The extension reads a secret from another region too.
API_KEY_SECRET_ARN = f"arn:aws:secretsmanager:{REGION}:{ACCOUNT}:secret:/dev/shared/serverless-sample-dd-api-key"

# Same handlers as the default handlers of the self-monitoring functions.
RUNTIMES = {
    "python": {
        "runtime": "python3.13",
        "file": "index.py",
        "code": 'def handler(_event, _context):\n    return {"statusCode": 200, "body": "hello, world"}\n',
    },
    "python314": {
        "runtime": "python3.14",
        "file": "index.py",
        "code": 'def handler(_event, _context):\n    return {"statusCode": 200, "body": "hello, world"}\n',
    },
    "node": {
        "runtime": "nodejs24.x",
        "file": "index.js",
        "code": 'exports.handler = async function () {\n  return { statusCode: 200, body: "hello, world" };\n};\n',
    },
}

BASE_ENV = {
    "DD_API_KEY_SECRET_ARN": API_KEY_SECRET_ARN,
    "DD_SITE": "datadoghq.com",
    "DD_SERVICE": "extension-perf-test",
    "DD_ENV": "test",
}


def owner():
    return re.sub(r"[^a-z0-9]+", "-", os.environ.get("USER", "unknown").lower()).strip("-")


def zip_bytes(rt):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        z.writestr(zipfile.ZipInfo(rt["file"]), rt["code"])
    return buf.getvalue()


def ensure_role(iam):
    name = f"{owner()}-extension-perf-role"
    try:
        return iam.get_role(RoleName=name)["Role"]["Arn"]
    except iam.exceptions.NoSuchEntityException:
        pass
    arn = iam.create_role(
        RoleName=name,
        AssumeRolePolicyDocument=json.dumps({
            "Version": "2012-10-17",
            "Statement": [{"Effect": "Allow", "Principal": {"Service": "lambda.amazonaws.com"}, "Action": "sts:AssumeRole"}],
        }),
    )["Role"]["Arn"]
    iam.attach_role_policy(RoleName=name, PolicyArn="arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole")
    iam.put_role_policy(
        RoleName=name,
        PolicyName="read-api-key",
        PolicyDocument=json.dumps({
            "Version": "2012-10-17",
            "Statement": [{"Effect": "Allow", "Action": "secretsmanager:GetSecretValue", "Resource": API_KEY_SECRET_ARN + "*"}],
        }),
    )
    print(f"created role {name}, waiting for IAM to propagate", flush=True)
    time.sleep(12)
    return arn


def ensure_function(lam, name, rt, arch, role_arn, layers):
    try:
        lam.get_function(FunctionName=name)
    except lam.exceptions.ResourceNotFoundException:
        lam.create_function(
            FunctionName=name,
            Runtime=rt["runtime"],
            Architectures=[arch],
            Role=role_arn,
            Handler="index.handler",
            Code={"ZipFile": zip_bytes(rt)},
            Timeout=30,
            MemorySize=1024,
            Layers=layers,
            Environment={"Variables": BASE_ENV},
        )
        lam.get_waiter("function_active_v2").wait(FunctionName=name)
    # As in the self-monitoring functions. Without this and the pause between warm invocations,
    # some warm invocations started a new execution environment.
    lam.put_function_concurrency(FunctionName=name, ReservedConcurrentExecutions=1)


def invoke(lam, fn):
    # A DNS failure on the local machine once stopped a 35-minute run, so retry connection errors.
    for attempt in range(5):
        try:
            r = lam.invoke(FunctionName=fn, Payload=b"{}")
            break
        except BotoConnectionError as e:
            print(f"{fn}: {e}, retrying", flush=True)
            time.sleep(5 * (attempt + 1))
    else:
        return
    if r.get("FunctionError"):
        print(f"{fn}: function error: {r['Payload'].read()[:300]!r}", flush=True)


def collect_logs(logs, name, start_ms, expected_reports):
    """Return the log events of one function. Stream names get the function name as a prefix."""
    """Poll CloudWatch until every invocation's REPORT line arrived, or 3 minutes passed."""
    deadline = time.time() + 180
    while True:
        events = []
        for page in logs.get_paginator("filter_log_events").paginate(
            logGroupName=f"/aws/lambda/{name}", startTime=start_ms
        ):
            events += [
                {"timestamp": e["timestamp"], "logStreamName": f"{name}/{e['logStreamName']}", "message": e["message"]}
                for e in page["events"]
            ]
        reports = sum(e["message"].startswith("REPORT") for e in events)
        if reports >= expected_reports or time.time() > deadline:
            return events, reports
        time.sleep(10)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("variants", nargs="+", help="NAME=SPEC, where SPEC is none, v<N> or a layer ARN")
    p.add_argument("--out", required=True, help="directory for <variant>.jsonl and meta.json")
    p.add_argument("-n", type=int, default=20, help="cold starts per variant")
    p.add_argument("--warm", type=int, default=0, help="warm invocations after each cold start")
    p.add_argument("--warm-interval", type=float, default=1, help="seconds between warm invocations")
    p.add_argument("--runtime", default="python", choices=sorted(RUNTIMES))
    p.add_argument("--arch", default="arm64", choices=["arm64", "x86_64"])
    p.add_argument("--fips", action="store_true", help="resolve v<N> to the FIPS layer")
    p.add_argument("--region", default=REGION, help="region of the functions and of every layer")
    p.add_argument("--copies", type=int, default=1, help="functions per variant, to get more samples in the same time")
    p.add_argument("--env", action="append", default=[], help="KEY=VALUE for every variant")
    args = p.parse_args()

    rt = RUNTIMES[args.runtime]
    layer_arch = "arm64" if args.arch == "arm64" else "amd64"
    env = {**BASE_ENV, **dict(kv.split("=", 1) for kv in args.env)}
    variants = {}
    for v in args.variants:
        vname, spec = v.split("=", 1)
        variants[vname] = [] if spec == "none" else [resolve(spec, layer_arch, args.fips, args.region)]

    lam = boto3.client("lambda", region_name=args.region)
    logs = boto3.client("logs", region_name=args.region)
    role_arn = ensure_role(boto3.client("iam"))
    os.makedirs(args.out, exist_ok=True)

    meta = {"args": vars(args), "env": env, "variants": {}}
    for vname, layers in variants.items():
        fns = []
        for k in range(1, args.copies + 1):
            fn = f"{owner()}-extperf-{args.runtime}-{args.arch}-{vname}" + (f"-{k}" if args.copies > 1 else "")
            ensure_function(lam, fn[:64], rt, args.arch, role_arn, layers)
            fns.append(fn[:64])
        sizes = layer_sizes(lam, layers[0]) if layers else (0, 0)
        meta["variants"][vname] = {"functions": fns, "layers": layers, "zipped_bytes": sizes[0], "unzipped_bytes": sizes[1]}

    start_ms = int(time.time() * 1000)
    all_fns = [fn for v in meta["variants"].values() for fn in v["functions"]]
    try:
        with ThreadPoolExecutor(max_workers=16) as pool:
            for i in range(1, args.n + 1):
                for v in meta["variants"].values():
                    # A changed environment variable makes Lambda start a new execution environment.
                    for fn in v["functions"]:
                        run_env = {**env, "RUN_ID": f"{i}-{random.randint(0, 1 << 30)}"}
                        lam.update_function_configuration(FunctionName=fn, Layers=v["layers"], Environment={"Variables": run_env})
                for fn in all_fns:
                    lam.get_waiter("function_updated_v2").wait(FunctionName=fn)
                # All functions are invoked together at a fixed cadence, so a long --warm-interval
                # costs the same for any number of variants and copies.
                step_start = time.time()
                for j in range(1 + args.warm):
                    time.sleep(max(0, step_start + j * args.warm_interval - time.time()))
                    list(pool.map(lambda fn: invoke(lam, fn), all_fns))
                print(f"round {i}/{args.n} done", flush=True)
    finally:
        # Save what CloudWatch has, also after an error, so that a long run is not lost.
        for vname, v in meta["variants"].items():
            events, reports = [], 0
            for fn in v["functions"]:
                fn_events, fn_reports = collect_logs(logs, fn, start_ms, args.n * (1 + args.warm))
                events += fn_events
                reports += fn_reports
            with open(os.path.join(args.out, f"{vname}.jsonl"), "w") as f:
                f.writelines(json.dumps(e) + "\n" for e in events)
            print(f"{vname}: saved {len(events)} log events, {reports} REPORT lines", flush=True)
        with open(os.path.join(args.out, "meta.json"), "w") as f:
            json.dump(meta, f, indent=2)


if __name__ == "__main__":
    main()
