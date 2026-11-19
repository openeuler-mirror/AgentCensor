"""Sort private text locally; output only positions, never operand values."""
import json
import sys


inputs = json.load(sys.stdin)["inputs"]
json.dump({"order": sorted(range(len(inputs)), key=lambda i: inputs[i]["value"])}, sys.stdout)
