# TASKS

- [x] Confirm W32Time has no NTS support; vet existing Windows NTS clients (rejected: unlicensed, tiny, unreviewed).
- [x] Build client on ntp-proto 1.9.0 (ntpd-rs); confirm it builds on Windows.
- [x] Dry run against 5 public NTS servers: all authenticated, all agree; offset matches w32tm stripchart (-0.844 s, 2026-11-12).
- [x] Unit tests for the agreement logic (5 passing).
- [x] --apply without privilege fails closed.
- [x] install.ps1 on the dev machine: first run stepped -0.847 s (measured -0.847 s); W32Time NoSync, STS off, tasks disabled.
- [x] Slew direction was inverted in the first build (requested -0.000235 s, measured +0.000229 s). `--slew-test`: adjustment inc+5000 gave -5.0 ms over 10 s, inc-5000 gave +5.0 ms. Fixed in `adjustment_for`; misses of over 50% now step the remainder.
- [x] After the fix: scheduled slews match (-2.381 ms requested / -2.381 ms measured; -0.186 / -0.186); NIST within 1 ms.
- [x] Public repo prep: MIT OR Apache-2.0, THIRD-PARTY.md, CI + attested release workflow, install.ps1 works from a release zip or a source checkout, -DryRun/-Servers/-IntervalMinutes.
- [x] CI green on windows-latest, including a live dry run (5/5 servers agree on the runner). v0.1.0 released 2026-11-12; downloaded zip matches SHA256SUMS, and `gh attestation verify` passes (built by ci.yml @ v0.1.0 from a0e83d7).
- [ ] Test install.ps1 under Windows PowerShell 5.1 (only tested under pwsh 7 so far).
- [ ] Reports from other Windows builds (only Windows 11 26200 tested).
