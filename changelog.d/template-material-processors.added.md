- Template material processors: `block_age`, `blackstone_replace` and `lava_submerged_block` now run in
  the ordered processor pipeline, preserving vanilla's block properties, outline-shape checks, NBT and
  placement-settings random consumption. Complete processing passes are compared with vanilla, including
  every block state and interactions with gravity, rules, filters and capped finalizers. Jigsaw replacement
  and template placement on chunk workers follow separately (`harpas-worldgen`).
