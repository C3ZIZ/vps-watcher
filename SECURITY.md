# Security policy

vps-watcher runs on servers and reads sensitive data such as logs, process lists and container details, so security reports get priority.

## Report a vulnerability

Please do not open a public issue. Use GitHub's private reporting instead: **Security → Report a vulnerability** on this repository.

Include the version (`vps-watcher version`), what an attacker can do, and the steps to reproduce it. You will get a reply within a few days. Once a fix is released, you will be credited unless you prefer otherwise.

## Supported versions

Only the latest release receives fixes. Upgrade by running the install command again.

## Design notes

- The UI listens on `127.0.0.1` by default; HTTPS is the reverse proxy's job.
- The first admin can be created only over a direct localhost connection.
- The service is read-only towards your apps: it sends only `GET` requests to Docker and never starts, stops or changes anything.
- Membership in the `docker` group is effectively root on most hosts. The installer adds the service user to it so containers can be listed. If you do not want that, remove the user from the group; containers then disappear from the dashboard and everything else keeps working.
