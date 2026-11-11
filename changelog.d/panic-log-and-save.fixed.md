- A panic is written to `logs/latest.log` as an error, with its thread and place, instead of only to the
  terminal. When the logic thread panics, the world is saved before the server stops, and the server reports
  why it stopped (`harpas-engine`, `harpas-server`).
