- Breaking blocks in Creative: the server checks reach and height as vanilla, leaves the fluid of a waterlogged
  block, updates the heightmaps, removes the block's block entity, saves the change, sends it to every player who
  has the chunk with the particles and sound of the broken block, and acknowledges the client's prediction.
  Survival breaking, neighbor updates and the light of changed blocks follow (`harpas-engine`).
