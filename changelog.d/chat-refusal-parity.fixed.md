- Chat that the server does not accept gets vanilla's answers, in the client's language, where it got texts in
  English: a red system message for a message out of order, unsigned in a signing session, wrongly signed,
  after a broken signing chain or signed with an expired key, and vanilla's disconnection reasons for a chat
  key that Mojang did not certify and for a bad acknowledgement (`harpas-java`).
- A chat key that had expired when it arrived no longer disconnects a player who has no session, who is left
  without one, as in vanilla; a player who was signing is disconnected. A player who sends again the key it
  already signs with keeps its signing chain, where it was disconnected for a repeated session
  (`harpas-java`, `harpas-engine`).
- With `enforce-secure-profile=false`, a player without a chat session who sends a signed message has it
  relayed unsigned, as in vanilla, where it was refused; such a player's messages are no longer held to the
  order of their timestamps (`harpas-java`).
