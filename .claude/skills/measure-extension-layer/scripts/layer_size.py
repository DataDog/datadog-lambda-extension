"""Print the zipped and unzipped size of extension layer versions.

Usage (through aws-vault, so boto3 gets credentials):
  aws-vault exec sso-serverless-sandbox-account-admin -- python3 layer_size.py v100 v101 <layer-arn> ...
  aws-vault exec sso-serverless-sandbox-account-admin -- python3 layer_size.py --zip .layers/datadog_extension-arm64.zip

A spec is a released version (`v101`) or a full layer version ARN.
"""

import argparse
import io
import urllib.request
import zipfile

import boto3

REGION = "us-east-2"
# The AWS account that publishes the released extension layers.
RELEASE_ACCOUNT = "464622532012"


def released_arn(version, arch="arm64", fips=False, region=REGION):
    name = "Datadog-Extension" + ("-ARM" if arch == "arm64" else "") + ("-FIPS" if fips else "")
    return f"arn:aws:lambda:{region}:{RELEASE_ACCOUNT}:layer:{name}:{version}"


def resolve(spec, arch="arm64", fips=False, region=REGION):
    """Turn `v101` into a released layer ARN. Return an ARN unchanged."""
    if spec.startswith("v") and spec[1:].isdigit():
        return released_arn(int(spec[1:]), arch, fips, region)
    return spec


def zip_sizes(data):
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        return len(data), sum(i.file_size for i in z.infolist())


def layer_sizes(lam, arn):
    """Return (zipped bytes, unzipped bytes) of one layer version."""
    content = lam.get_layer_version_by_arn(Arn=arn)["Content"]
    with urllib.request.urlopen(content["Location"]) as r:
        return zip_sizes(r.read())


def main():
    p = argparse.ArgumentParser()
    p.add_argument("specs", nargs="*", help="v<N> or a layer version ARN")
    p.add_argument("--zip", action="append", default=[], help="a local layer zip")
    p.add_argument("--arch", default="arm64", choices=["arm64", "amd64"])
    p.add_argument("--fips", action="store_true")
    args = p.parse_args()

    lam = boto3.client("lambda", region_name=REGION) if args.specs else None
    print(f"{'layer':80} {'zipped':>10} {'unzipped':>10}")
    for spec in args.specs:
        arn = resolve(spec, args.arch, args.fips)
        print(f"{arn:80} {'%10d %10d' % layer_sizes(lam, arn)}")
    for path in args.zip:
        with open(path, "rb") as f:
            print(f"{path:80} {'%10d %10d' % zip_sizes(f.read())}")


if __name__ == "__main__":
    main()
