- Java chunk encoding and compression shared between viewers through a cache bounded to 4,096 entries and
  64 MiB of frame buffers. Changes to chunk contents or light invalidate the cached packet, unloaded chunk data
  is released, and each connection encrypts its own stream (`harpas-java`, `harpas-server`).
