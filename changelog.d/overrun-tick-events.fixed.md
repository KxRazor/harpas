- A server whose ticks take longer than 50 milliseconds keeps applying what its players send, and stops when asked
  to. It read connection events only while waiting for the next tick, so it never read them once it fell behind
  (`harpas-engine`).
