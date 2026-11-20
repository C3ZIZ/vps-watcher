# Security policy

vps-watcher runs on servers and reads sensitive data such as logs, process lists and container details, so security reports get priority.

## Report a vulnerability

Please do not open a public issue. Use GitHub's private reporting instead: **Security → Report a vulnerability** on this repository. You can also email the maintainer, Abdulaziz Hafiz, at abdulazizhafez2004@gmail.com.

Include the version (`vps-watcher version`), what an attacker can do, and the steps to reproduce it. You will get a reply within a few days. Once a fix is released, you will be credited unless you prefer otherwise.

## Supported versions

Only the latest release receives fixes. Upgrade by running the install command again.

## Design notes

- The UI listens on `127.0.0.1` by default; HTTPS is the reverse proxy's job.
- The first admin can be created only over a direct localhost connection addressed to `localhost`, `127.0.0.1` or `[::1]`, which also blocks DNS-rebinding tricks through an SSH tunnel. Until you create the admin, any program already running on the server could create it first; finish setup right after installing.
- Logins: attempts are counted before the password check, IPv6 clients are limited per /64, and at most two password checks run at once.
- Requests: 16 KB body limit, 10 s to send headers, 30 s per request, at most 256 connections and 32 live streams.
- Log files are opened without following symbolic links and only if they are regular files, so a swapped file cannot leak other files or hang the service.
- The service is read-only towards your apps: it sends only `GET` requests to Docker and never starts, stops or changes anything.
- Membership in the `docker` group is effectively root on most hosts. The installer adds the service user to it so containers can be listed. If you do not want that, remove the user from the group; containers then disappear from the dashboard and everything else keeps working.

## Release signing

Each release's file list (`SHA256SUMS`) is signed with an Ed25519 key that the maintainer keeps offline and never stores on GitHub. Before signing, the maintainer verifies two things:

- each binary's GitHub build attestation, which shows it came from this repository's release workflow and the tagged commit
- every other file against the tagged source

`vps-watcher upgrade` and automatic updates verify the signature and every file hash, refuse older versions, and roll back if the new version does not stay up.

Limits:

- **The installer.** The one-line installer relies on HTTPS and checksums only. After the first install, use `sudo vps-watcher upgrade`.
- **The build.** The signature confirms the maintainer approved the files. It cannot catch a malicious change in the tagged source or its dependencies, or a compromised build workflow.
- **Held-back updates.** Someone who controls GitHub or a mirror can stop a server from seeing new releases, which delays updates. They still cannot install anything unsigned.
