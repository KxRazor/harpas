- Chunks past the edge of a world vanilla generated failed to load, because the chunks
  vanilla left at `initialize_light` could not take their neighbors' decorations. The chunk workers now resume them
  like chunks at `features`, and use chunks from `light` on as saved (`harpas-engine`).
