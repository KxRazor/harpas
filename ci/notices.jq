# Turns `cargo about generate --format json` into the Rust dependency section of THIRD_PARTY_NOTICES.md.
#
# MIT and similar licenses differ between crates only in their copyright lines, so the text of each license is
# printed once, without those lines, and every crate's copyright lines are listed above it. The template line
# of the Apache-2.0 appendix ("Copyright [yyyy] ...") stays in the text.

def is_copyright: test("^\\s*(Copyright|\\([cC]\\)|©)") and (test("\\[yyyy\\]") | not);
def crate_name: "`\(.crate.name) \(.crate.version)`";
# Texts that differ only in line wrapping or in a title line are the same license text.
def text_key: gsub("\\s+"; " ") | ascii_downcase | sub("^ *(the )?mit license( \\(mit\\))? *"; "") | ltrimstr(" ") | rtrimstr(" ");

[
  .licenses[]
  | (.text | split("\n")) as $lines
  | {
      id,
      name,
      body: ([$lines[] | select(is_copyright | not) | sub("\\s+$"; "")] | join("\n") | gsub("\n{3,}"; "\n\n") | ltrimstr("\n") | rtrimstr("\n")),
      copyright: [$lines[] | select(is_copyright) | sub("^\\s+"; "") | sub("\\s+$"; "")],
      crates: [.used_by[] | crate_name]
    }
  | .key = (.body | text_key)
]
| group_by(.id) as $by_id
| if length == 0 then
    "Harpas has no third-party Rust dependencies yet."
  else
    ([
      "| License | Used by |",
      "|---|---|",
      ($by_id[] | "| \(.[0].name) | \([.[].crates[]] | unique | join(" ")) |")
    ] | join("\n"))
    + "\n"
    + ([
        group_by([.id, .key])[]
        | "\n### \(.[0].name)\n\n"
          + ([.[] | .crates[] as $crate | "- \($crate)" + (if (.copyright | length) > 0 then ": " + (.copyright | join("; ")) else "" end)]
             | unique | join("\n"))
          + "\n\n```text\n\(.[0].body)\n```"
      ] | join("\n"))
  end
