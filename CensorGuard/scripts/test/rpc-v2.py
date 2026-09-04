#!/usr/bin/env python3
import argparse
import json
import socket
import time


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--socket", required=True)
    parser.add_argument("--method", required=True)
    parser.add_argument("--params", default="{}")
    parser.add_argument("--policy-file")
    args = parser.parse_args()

    params = json.loads(args.params)
    if args.policy_file:
        with open(args.policy_file, "r", encoding="utf-8") as source:
            params["policy_yaml"] = source.read()
    request = {
        "v": 2,
        "request_id": f"test-{time.time_ns()}",
        "method": args.method,
        "params": params,
    }
    payload = json.dumps(request, separators=(",", ":")).encode() + b"\n"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(10)
        client.connect(args.socket)
        client.sendall(payload)
        chunks = bytearray()
        while not chunks.endswith(b"\n"):
            chunk = client.recv(65536)
            if not chunk:
                raise RuntimeError("daemon closed the RPC stream")
            chunks.extend(chunk)
    response = json.loads(chunks)
    print(json.dumps(response, indent=2, sort_keys=True))
    if not response.get("ok"):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
