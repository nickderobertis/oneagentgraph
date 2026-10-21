#!/usr/bin/env python3
"""A onejudge command-side judge that accepts the worker's first turn.

The probe's two-party member is judged by this rather than by a harness so the
one real turn it spends is the agent's. It speaks onejudge's JSON-lines
protocol: one request object on stdin, one answer on stdout.
"""
import json
import sys

request = json.load(sys.stdin)
op = request.get("op")
if op == "supervisor":
    answer = {"completion": True, "reason": "probe: one worker turn is the whole probe"}
elif op == "user":
    answer = {"message": "done", "stop": True}
elif op == "judge" and request.get("kind") == "boolean":
    answer = {"value": True, "reason": "probe"}
elif op == "judge":
    answer = {"value": request.get("max") or 5, "reason": "probe"}
elif op == "assess":
    answer = {"text": "None"}
else:
    sys.stderr.write(f"probe judge: unknown op {op!r}\n")
    sys.exit(1)
print(json.dumps(answer))
