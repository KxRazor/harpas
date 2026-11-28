- Preserve worlds across interrupted region and map saves, reject NBT that cannot be read back, retain
  chunk metadata when editing blocks, and conserve items during inventory swaps and entity merges. Chests
  publish treasure maps only when their interaction can complete, and validate loot tables and their work
  before replacing saved loot seeds (`harpas-anvil`, `harpas-core`, `harpas-engine`).
