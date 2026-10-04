# Security policy

Security fixes are developed on the current `main` branch. Older snapshots and
third-party builds may lack fixes; a maintained release/version matrix has not
yet been established.

## Reporting a vulnerability

Use **Report a vulnerability** in the repository's GitHub Security tab if
private reporting is enabled. If it is unavailable, open an issue requesting a
private reporting channel without including exploit instructions, credentials
or private files. A private reporting channel should be enabled before release.

Include the Spotty version/commit, backend submodule commit, distribution,
native build details, affected feature and a minimal reproduction with
synthetic data. Explain the attacker-controlled input and likely impact.
Redact clipboard contents, API keys, paths and other personal data from logs.
Do not publish working exploit details before the maintainers can investigate.

See [Privacy and security](PRIVACY_AND_SECURITY.md) for permissions, data storage,
network behavior, the review findings and remaining limitations.
