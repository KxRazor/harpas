# Data

`registries-26.1.txt` lists the registries a Java 26.1 server synchronizes in the Configuration state and their
entries, in the order vanilla sends them. Each registry is a line, and its entries follow on lines indented by
two spaces. The entries' numeric IDs are their positions in the list.

It is built from `data/pc/26.1/loginPacket.json` of
[PrismarineJS/minecraft-data](https://github.com/PrismarineJS/minecraft-data) (MIT), commit
`bd8b5c6048e5af9694374b83f25f5b4aade619cc`, keeping only the names: the client takes the entries' data from its
built-in `minecraft:core` pack. To build it again:

```sh
curl -LO https://raw.githubusercontent.com/PrismarineJS/minecraft-data/bd8b5c6048e5af9694374b83f25f5b4aade619cc/data/pc/26.1/loginPacket.json
python3 -c 'import json
for r in json.load(open("loginPacket.json"))["dimensionCodec"].values():
    print(r["id"]); [print("  " + e["key"]) for e in r["entries"]]' > registries-26.1.txt
```

`tags-26.1.txt` lists, in the same format, the tags that the registry data of the client's `minecraft:core` pack
refers to: a 26.1 client refuses the registries when one of them is missing. Harpas sends them **empty** until
it has vanilla's data (M3, `docs/world.md`): enchantments then apply to no item, which a void world does not
notice. The list was taken from the errors a 26.1 client logs when it receives the registries without tags, and
completed with the names of vanilla tags that the client's item components refer to; an unused empty tag is
harmless.
