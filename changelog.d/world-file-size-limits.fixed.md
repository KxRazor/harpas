- A chunk, `level.dat` or a player's data that inflates to more than 256 MiB, or takes more than 1 GiB of
  memory once read, is refused as unreadable instead of filling the memory, as a crafted file in a downloaded
  world could (`harpas-anvil`, `harpas-core`).
