- A connection sends what the server has for its player as soon as it is ready, instead of up to 50 ms later
  when the player's client was sending nothing. Each connection now reads its socket on a second thread
  (`harpas-server`, `harpas-engine`).
