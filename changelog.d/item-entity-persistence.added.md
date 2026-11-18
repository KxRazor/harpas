- Dropped items survive chunk unloads and server restarts. Autosave and shutdown write vanilla entity regions,
  including stack components, UUID, position, velocity, age, health and pickup delay. Harpas also preserves
  the saved data of entity types and item components it cannot yet simulate (`harpas-engine`).
