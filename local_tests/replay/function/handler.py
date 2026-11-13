import json, os, time, urllib.request, uuid

UPSTREAM = os.environ["UPSTREAM_URL"]

def handler(event, context):
    # A nonce and a timestamp: values that differ on every run.
    url = f"{UPSTREAM}/items/{event['id']}?nonce={uuid.uuid4()}"
    item = json.load(urllib.request.urlopen(url))
    ip = urllib.request.urlopen("https://checkip.amazonaws.com").read().decode().strip()
    result = {"id": event["id"], "item": item, "caller_ip_octets": len(ip.split(".")),
              "generated_at": time.time()}
    if os.environ.get("CHANGED_BEHAVIOR"):
        # Simulates a code change: an extra upstream call and a new response field.
        urllib.request.urlopen(f"{UPSTREAM}/items/{event['id']}?expand=true").read()
        result["expanded"] = True
    return result
