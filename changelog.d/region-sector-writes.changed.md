- Saving writes only the chunks that changed into free sectors of their region, as vanilla does, instead of
  reading and rewriting the whole region file: 7 ms instead of 160 ms for one chunk of a full region on a
  disk. A save is still one transaction with the region's `.mcc` files (`harpas-anvil`, `harpas-engine`).
- A region with a chunk entry that cannot be read no longer stops the server when another of its chunks is
  saved: the entry is left as it is. Only an entry that points past the end of the file still does
  (`harpas-anvil`).
