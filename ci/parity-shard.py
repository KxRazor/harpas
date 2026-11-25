"""Select a disjoint nextest shard from the current inventory, longest estimates first."""

import json
from pathlib import Path
import sys

inventory, index, count = sys.argv[1:]
index, count = int(index), int(count)
if not 1 <= index <= count:
    sys.exit("shard index must be between 1 and the shard count")

weights = {}
for line in Path(__file__).with_name("parity-seconds.tsv").read_text().splitlines():
    if line and not line.startswith("#"):
        name, seconds = line.split()
        weights[name] = int(seconds)

tests = []
for binary, suite in json.loads(Path(inventory).read_text())["rust-suites"].items():
    for name, case in suite["testcases"].items():
        if case["filter-match"]["status"] == "matches":
            seconds = weights.get(f"{binary}/{name}", weights.get(binary, 25))
            tests.append((seconds, binary, name))

shards = [[] for _ in range(count)]
loads = [0] * count
for seconds, binary, name in sorted(tests, reverse=True):
    shard = min(range(count), key=loads.__getitem__)
    shards[shard].append(f"(binary_id(={binary}) & test(={name}))")
    loads[shard] += seconds

selected = shards[index - 1]
if not selected:
    sys.exit("empty parity shard")
print(" | ".join(selected))
