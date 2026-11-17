- Player inventories survive reconnects and server restarts: main inventory, hotbar, armor, offhand and the
  selected slot are loaded and saved in vanilla player data by a worker, on autosave, departure and shutdown.
  Shutdown closes live menus before saving, returning their cursor and ingredients to inventory slots.
  Other player and item fields are retained, backup recovery is supported, and a rapid reconnect sees the
  newest inventory even while its save is pending. Saved-NBT/network codecs for 26 item components, including
  names, damage and enchantments, are compared with vanilla; unported component values are refused without
  stripping items or overwriting the player's original data (`harpas-engine`, `harpas-java`, `harpas-server`).
