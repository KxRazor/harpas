- The light of generated chunks is computed on worker threads as soon as a chunk and its neighbors are loaded,
  instead of on the connection of the first player it is sent to, and a chunk is sent once its light is ready
  (`harpas-engine`, `harpas-server`).
