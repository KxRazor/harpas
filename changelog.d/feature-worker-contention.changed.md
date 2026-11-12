- With `generation.features`, chunk workers no longer wait for each other while there are other chunks to
  generate around them: a view of 441 chunks generates at 57 chunks a second with 8 workers instead of 35,
  and at 45 with 4 instead of 22 (`harpas-engine`).
