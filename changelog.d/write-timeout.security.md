- A connection whose client stops reading is closed after 30 seconds. It used to wait forever, keeping its thread
  and the player online while the packets for it accumulated in memory (`harpas-server`).
