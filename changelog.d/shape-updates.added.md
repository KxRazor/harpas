- Shape updates: when a block changes, its neighbors take their new shapes in vanilla's order, depth first, so a
  torch, a plant or a door breaks with the block that held it, with its particles and sound, a grass block stops
  being snowy when its snow is gone, and fences, walls, panes and stairs reconnect. Each block follows the rules
  ported for world generation; neighbor updates, which redstone reacts to, follow (`harpas-engine`).
