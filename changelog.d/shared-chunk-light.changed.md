- The light of a generated chunk is computed once and shared by the connections that send it, until the chunk or
  one of its neighbors changes. Every connection used to compute it again for every chunk it sent, which took far
  longer than the rest of the chunk packet (`harpas-server`).
