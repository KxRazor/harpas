- Faster startup: the Overworld's generator and its structure placement share one structure state, whose
  stronghold ring positions are computed once instead of up to three times, so preparing world generation takes
  about 6 seconds instead of 16 (`harpas-engine`, `harpas-server`).
