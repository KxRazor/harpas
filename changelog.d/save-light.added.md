- Saved light: with `generation.features`, a final chunk is saved with the light the chunk workers computed for it,
  at vanilla's `light` status with `isLightOn`, so vanilla keeps that light instead of lighting the chunk again and
  spawns the chunk's first mobs when it opens the world (`harpas-engine`).
