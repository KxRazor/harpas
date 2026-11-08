# Data

`blocks-26.1.txt` lists the blocks of Java 26.1 and their states, in the order of the block state registry.
Each block is a line: its name without the `minecraft:` namespace, `default=<n>` when its default state is not
its first, then each property as `name=value,value,…`, then `emit=<n>` and `filter=<n>` for the block's default
light emission and dampening. A block's states take consecutive IDs from the first free one: the properties are
in vanilla's order, sorted by name, and the last one changes fastest. Boolean properties list `true` before
`false`, as vanilla orders them. State-dependent light values are applied by `BlockStates` on top of these
defaults.

It is built from `data/pc/26.1/blocks.json` of
[PrismarineJS/minecraft-data](https://github.com/PrismarineJS/minecraft-data) (MIT), commit
`bd8b5c6048e5af9694374b83f25f5b4aade619cc`, the revision the Java codecs were built from. To build it again:

```sh
curl -LO https://raw.githubusercontent.com/PrismarineJS/minecraft-data/bd8b5c6048e5af9694374b83f25f5b4aade619cc/data/pc/26.1/blocks.json
python3 -c 'import json
for b in json.load(open("blocks.json")):
    line = [b["name"]]
    if b["defaultState"] != b["minStateId"]:
        line.append("default=%d" % (b["defaultState"] - b["minStateId"]))
    for s in b["states"]:
        line.append(s["name"] + "=" + ",".join(s.get("values") or ["true", "false"]))
    line.append("emit=%d" % b["emitLight"])
    line.append("filter=%d" % b["filterLight"])
    print(" ".join(line))' > blocks-26.1.txt
```

`block-shapes-26.1.txt` holds the collision shape of every block state of Java 26.1, as a block has it with no
neighbors. Its `shape` lines list the distinct shapes, numbered from 0 in order: each is a list of boxes, six
numbers per box (minimum X, Y and Z, then maximum X, Y and Z) in 1/32 of a block, which every coordinate is a
multiple of. Some boxes reach past the block, such as a fence's 1.5 block height. The other lines follow the
blocks of `blocks-26.1.txt` in order: the block's name, then the shape of each of its states in state order, with
`<shape>*<n>` for `n` consecutive states sharing one. `BlockBehavior` in `harpas-core` derives the solid,
motion-blocking and full-face flags from these shapes.

It is built from `data/pc/26.1/blockCollisionShapes.json` and `data/pc/26.1/blocks.json` of the same
minecraft-data revision. To build it again:

```sh
curl -LO https://raw.githubusercontent.com/PrismarineJS/minecraft-data/bd8b5c6048e5af9694374b83f25f5b4aade619cc/data/pc/26.1/blockCollisionShapes.json
curl -LO https://raw.githubusercontent.com/PrismarineJS/minecraft-data/bd8b5c6048e5af9694374b83f25f5b4aade619cc/data/pc/26.1/blocks.json
python3 -c 'import json
shapes = json.load(open("blockCollisionShapes.json"))
for i in range(len(shapes["shapes"])):
    print(" ".join(["shape"] + [str(round(v * 32)) for b in shapes["shapes"][str(i)] for v in b]))
for b in json.load(open("blocks.json")):
    s = shapes["blocks"][b["name"]]
    states = s if isinstance(s, list) else [s] * (b["maxStateId"] - b["minStateId"] + 1)
    runs = []
    for v in states:
        if runs and runs[-1][0] == v:
            runs[-1][1] += 1
        else:
            runs.append([v, 1])
    print(" ".join([b["name"]] + [str(v) if c == 1 else "%d*%d" % (v, c) for v, c in runs]))' > block-shapes-26.1.txt
```
