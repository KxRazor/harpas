# Changelog entries

Each pull request that changes behavior adds its [CHANGELOG.md](../CHANGELOG.md) entry here as its own file, so
pull requests open at the same time never change the same lines. A release moves them into `CHANGELOG.md` with
`ci/changelog.sh` ([docs/releases.md](../docs/releases.md#changelog)).

A file is named `<name>.<section>.md`:

- `<name>` is short, lowercase and hyphenated, usually the task part of the branch name: `ice-features`.
- `<section>` is the `CHANGELOG.md` subsection: `added`, `changed`, `fixed`, `removed` or `security`.

It holds one entry, written as it will read in `CHANGELOG.md`:

```markdown
- Frozen biomes: the `spike` feature of ice spike plains, `iceberg` with packed and blue ice icebergs centered at
  the sea level, and `blue_ice`, which grows under packed ice in frozen oceans (`harpas-worldgen`).
```

A change that belongs to more than one subsection adds one file for each.
