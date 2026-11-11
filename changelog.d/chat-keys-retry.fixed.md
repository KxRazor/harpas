- When Minecraft Services' chat keys cannot be fetched at startup, the server asks again every minute, and
  until they load it ignores the players' chat sessions, as vanilla does without the keys. Every online-mode
  player used to be disconnected on joining until the server was restarted (`harpas-java`, `harpas-server`).
