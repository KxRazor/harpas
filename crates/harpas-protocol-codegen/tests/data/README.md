# Test data

`minecraft-data-26.1.json` is a subset of `data/pc/26.1/protocol.json` from
[PrismarineJS/minecraft-data](https://github.com/PrismarineJS/minecraft-data) (MIT), commit
`bd8b5c6048e5af9694374b83f25f5b4aade619cc`: the native types, the types the Handshaking, Status and Login
states use, and those three states unchanged. Configuration and Play are kept as empty objects.

To check the schema against the full file instead:

```sh
curl -LO https://raw.githubusercontent.com/PrismarineJS/minecraft-data/bd8b5c6048e5af9694374b83f25f5b4aade619cc/data/pc/26.1/protocol.json
cargo run -p harpas-protocol-codegen -- check crates/harpas-protocol-java/schema/26.1 protocol.json
```
