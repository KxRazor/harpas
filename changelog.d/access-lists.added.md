- Bans, the whitelist and operators from vanilla's `banned-players.json`, `banned-ips.json`, `whitelist.json` and
  `ops.json`: logins are refused in vanilla's order with its messages when `white-list=true` or a ban matches,
  and operators with `bypassesPlayerLimit` join a full server. The files are written back at startup as vanilla
  writes them (`harpas-server`).
