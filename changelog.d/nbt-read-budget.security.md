- Network NBT is limited to 2 MiB of memory once read, as vanilla's quota for packets. A small compressed packet
  holding a list of empty compounds could make the server allocate hundreds of megabytes (`harpas-core`).
