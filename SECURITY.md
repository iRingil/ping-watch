# Security Policy

## Supported versions

Only the latest release receives security fixes.

## Known limitations

- **Running as administrator.** TCP probes need administrator rights, and then the whole app runs with them. A normal
  installation and a portable copy live in folders the user's own programs may write to, so a program running as that
  user could replace `PingWatch.exe` before it is started as administrator. Ping Watch loads DLLs only from System32,
  so a DLL put next to it isn't picked up. If that matters to you, use ICMP probes, which need no rights.
- **Single-instance objects.** The named event that asks Ping Watch to quit (used by the installer) can be signaled by
  any program of the same Windows session.

## Reporting a vulnerability

Please **do not** open a public issue.
Use [private vulnerability reporting](https://github.com/iRingil/ping-watch/security/advisories/new) instead.

Include the app version, steps to reproduce, and the potential impact.
This is a solo-maintained project; reports are handled on a best-effort basis.
