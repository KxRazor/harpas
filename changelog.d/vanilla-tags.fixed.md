- Java clients receive vanilla's tags in the Configuration state, resolved from the vanilla data at startup:
  661 tags of 15 registries, compared with a vanilla 26.1 server. They were sent empty, so a player under water
  saw no blue fog, and blocks, items, fluids and entities were in none of their tags. The tags of blocks, items,
  fluids and other built-in registries need the registry IDs that the installation generates with Java; for an
  installation made without it, run `harpas vanilla-data generate-registry` (`harpas-java`, `harpas-server`).
