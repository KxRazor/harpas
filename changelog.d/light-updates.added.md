- Light follows block changes: a block that changes how much light it lets through or gives off has the chunks
  within 15 blocks of it lit again on the light workers, the same way their light is first computed. The new light
  is saved and sent to the players at whose view's edge each chunk is, as vanilla sends it; a chunk saved before it
  is lit again is saved without `isLightOn`, so that vanilla lights it when it opens the world (`harpas-engine`).
