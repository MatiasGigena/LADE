# Security policy

## Supported versions

LADE is pre-release software. There are no released versions yet; only the current `main` branch is supported, and
fixes land there.

## Reporting a vulnerability

Please **do not open a public issue** for a security problem. Report it privately through GitHub's private
vulnerability reporting: on the [repository page](https://github.com/MatiasGigena/LADE), open the **Security** tab
and choose **Report a vulnerability**. This creates a private security advisory visible only to the maintainers.

Include as much of the following as you can:

- the affected commit (or bundle version, `laded --version`) and the host (distribution, kernel, bubblewrap, Git,
  Node and GTK versions);
- the component involved (service, sandbox/launcher, egress gateway, credentials, worker, desktop client,
  installer/bundle);
- steps to reproduce, ideally with a minimal repository or script, and what you expected instead;
- the impact you believe it has (for example sandbox escape, egress to a non-allowlisted host, credential exposure,
  integration without approval);
- any logs, with secrets removed. `laded diagnostics export FILE` writes a redacted export, but redaction is best
  effort: read it before attaching it.

Reports are handled on a best-effort basis; no response or fix time is guaranteed. Please give the maintainers a
reasonable chance to address the issue before disclosing it publicly.

## Scope

The execution boundary is documented in [execution-safety.md](docs/implementation/execution-safety.md). In short:

- **Sandbox.** Every agent, contained terminal and verification check runs under bubblewrap with all namespaces
  unshared, all capabilities dropped, a read-only root, a private `/tmp`, `prlimit` limits, a seccomp denylist and,
  when the systemd unit delegates cgroups, per-Objective cgroup v2 limits. The agent can write only its Objective's
  worktree and session directory; the repository's `.git` is read-only. The service's control socket, capability
  file, database and other Objectives are not reachable. If the sandbox cannot be created, execution is refused.
- **Egress gateway.** The sandbox has no network of its own. The worker reaches the network only through a
  per-launch `CONNECT` proxy that allows exact provider `host:port` pairs (port 443 by default) and refuses private,
  loopback and other special-purpose addresses.
- **Credentials.** Keys come from `laded`'s environment, the user's Secret Service, or a `credentials.env` that must
  be a `0600` regular file owned by the user. Only the selected provider's variables are released to the sandbox,
  and values are redacted from logs, errors and debug output. `laded credentials set` reads keys only from stdin.

Within that scope, examples of what we want to hear about: escaping the sandbox or writing outside the worktree,
reaching a host the policy does not allow, a key appearing in logs, diagnostics or another Objective, the desktop
client or a worker obtaining authority the service did not grant, or an integration happening without an approval
for the exact candidate.

## Known limitations (not vulnerabilities)

These are documented, accepted gaps; reports that only restate them are not needed, but ways to go beyond them are.
See [limitations.md](docs/implementation/limitations.md) for the full list.

- The released provider key is readable by every process in the sandbox, including tools the agent runs and
  approved extensions; there is no per-extension secrecy.
- The egress allowlist is per provider host, not per request: code in the sandbox can use the tunnel with the
  released key, and content sent to an allowed host is not inspected (no data-loss prevention). Through an upstream
  `HTTPS_PROXY`, private-address checks apply only to IP literals.
- The Secret Service has no per-application access control: any process in the user's unlocked session can read
  LADE's keyring items. The environment and `credentials.env` are plaintext options.
- The seccomp filter is a denylist; `ptrace` and `io_uring` are not filtered. The remaining kernel attack surface is
  that of an unprivileged user namespace.
- Disk use is a periodic monitor, not a quota; without delegated cgroups, memory is bounded only by virtual address
  space and processes per sandbox by `RLIMIT_NPROC` (not enforced for root). There is no CPU ceiling and no
  aggregate limit across Objectives.
- A supervisor that dies in a microsecond window after opening the launch gate can leave a sandbox running until
  the next `laded` start or the unit stops.
- Frames from the worker are schema-validated, but their content (agent activity) is untrusted and not otherwise
  filtered.
- Interactive OAuth logins and refresh-token custody are not implemented (restricted).
- There is no release signing key or trust bootstrap yet: an installed bundle is verified against its own checksum
  manifest, and a signature only against an allowed-signers file you configure yourself.
- LADE has not been validated as an installed release on the supported platform.
