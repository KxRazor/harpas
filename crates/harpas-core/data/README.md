# Data

`blocks-26.1.txt` lists the blocks of Java 26.1 and their states, in the order of the block state registry.
Each block is a line: its name without the `minecraft:` namespace, `default=<n>` when its default state is not
its first, then each property as `name=value,value,…`. A block's states take consecutive IDs from the first
free one: the properties are in vanilla's order, sorted by name, and the last one changes fastest. Boolean
properties list `true` before `false`, as vanilla orders them.

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
    print(" ".join(line))' > blocks-26.1.txt
```
