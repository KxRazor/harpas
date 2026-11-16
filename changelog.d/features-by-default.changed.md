- New chunks always run the FEATURES stage, as in vanilla: ores, vegetation, trees, lakes, springs and every
  structure of the Overworld are placed before players receive them, and `generation.features` in `harpas.toml`
  is gone. `generate-structures=false` still leaves structures out (`harpas-server`).
