# Security policy

Harpas is a server that accepts connections from the internet, so vulnerabilities matter even before the first
release. Thank you for reporting them privately.

## Supported versions

While Harpas is on `0.x`, only the latest stable release receives fixes, including security fixes
([docs/releases.md](docs/releases.md#support-policy)). Reports about the `main` branch are welcome too.

## Reporting a vulnerability

**Do not open a public issue, pull request or discussion for a vulnerability.**

Report it privately through GitHub:
[Report a vulnerability](https://github.com/KxRazor/harpas/security/advisories/new). Only the maintainers can
see the report.

Include what you can of:

- the affected component (for example the Java protocol, NetherNet, the JVM bridge) and version or commit;
- what an attacker can do: crash or stall the server, read or change data, run code, impersonate a player;
- steps or a proof of concept to reproduce it;
- whether it needs an account, operator rights, a plugin or a specific configuration.

Examples of what counts: packets that crash the server or exhaust its memory or CPU, authentication or
encryption bypasses, ways for a player to read or change what they should not, and escapes from the plugin
sandbox.

## What happens next

1. A maintainer acknowledges the report and may ask for details.
2. The fix is developed in a private advisory, and you are credited unless you prefer not to be.
3. The fix ships as a security release, a PATCH on the latest release with a `Security` changelog entry
   ([docs/releases.md](docs/releases.md#security-release)), and the advisory is published.

Please give the maintainers a reasonable time to release a fix before disclosing the issue publicly.
