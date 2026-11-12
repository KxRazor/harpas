- A tick no longer computes every player's view again: views and the chunks kept loaded are computed when a
  player enters another chunk, changes its view distance, joins or leaves. With 100 players at view distance
  32 standing still, the chunk part of a tick takes 0.2 ms instead of 10 ms. Player profiles, skins included,
  are no longer copied each time a player moves (`harpas-engine`).
