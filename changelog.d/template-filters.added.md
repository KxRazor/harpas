- Structure template block filters: deterioration (`block_rot`), protected terrain (`protected_blocks`),
  ignored block types (`block_ignore`) and passthrough (`nop`), combined with rules and capped finalizers in
  an ordered pipeline. All 40 vanilla processor lists compile and are compared with vanilla's own pipeline,
  including block filtering, NBT and placement randomness (`harpas-worldgen`). Jigsaw template placement on
  chunk workers follows separately.
