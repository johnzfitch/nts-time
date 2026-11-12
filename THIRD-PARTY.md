# Third-party crates

The Windows release binary statically links these crates (from `cargo tree -e normal --target x86_64-pc-windows-msvc`). Each is used under its own license; full texts are in each crate's source on crates.io.

| Crate | Version | License |
|:--|:--|:--|
| [`aead`](https://crates.io/crates/aead) | 0.5.2 | MIT OR Apache-2.0 |
| [`aes`](https://crates.io/crates/aes) | 0.8.4 | MIT OR Apache-2.0 |
| [`aes-siv`](https://crates.io/crates/aes-siv) | 0.7.0 | Apache-2.0 OR MIT |
| [`aws-lc-rs`](https://crates.io/crates/aws-lc-rs) | 1.19.0 | ISC AND (Apache-2.0 OR ISC) |
| [`aws-lc-sys`](https://crates.io/crates/aws-lc-sys) | 0.47.0 | ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0) |
| [`block-buffer`](https://crates.io/crates/block-buffer) | 0.10.4 | MIT OR Apache-2.0 |
| [`bytes`](https://crates.io/crates/bytes) | 1.12.1 | MIT |
| [`cfg-if`](https://crates.io/crates/cfg-if) | 1.0.5 | MIT OR Apache-2.0 |
| [`cipher`](https://crates.io/crates/cipher) | 0.4.4 | MIT OR Apache-2.0 |
| [`cmac`](https://crates.io/crates/cmac) | 0.7.2 | MIT OR Apache-2.0 |
| [`cpufeatures`](https://crates.io/crates/cpufeatures) | 0.2.17 | MIT OR Apache-2.0 |
| [`crypto-common`](https://crates.io/crates/crypto-common) | 0.1.7 | MIT OR Apache-2.0 |
| [`ctr`](https://crates.io/crates/ctr) | 0.9.2 | MIT OR Apache-2.0 |
| [`dbl`](https://crates.io/crates/dbl) | 0.3.2 | MIT OR Apache-2.0 |
| [`digest`](https://crates.io/crates/digest) | 0.10.7 | MIT OR Apache-2.0 |
| [`generic-array`](https://crates.io/crates/generic-array) | 0.14.7 | MIT |
| [`getrandom`](https://crates.io/crates/getrandom) | 0.2.17 | MIT OR Apache-2.0 |
| [`inout`](https://crates.io/crates/inout) | 0.1.4 | MIT OR Apache-2.0 |
| [`itoa`](https://crates.io/crates/itoa) | 1.0.19 | MIT OR Apache-2.0 |
| [`log`](https://crates.io/crates/log) | 0.4.35 | MIT OR Apache-2.0 |
| [`md-5`](https://crates.io/crates/md-5) | 0.10.6 | MIT OR Apache-2.0 |
| [`memchr`](https://crates.io/crates/memchr) | 2.8.3 | Unlicense OR MIT |
| [`mio`](https://crates.io/crates/mio) | 1.2.4 | MIT |
| [`ntp-proto`](https://crates.io/crates/ntp-proto) | 1.9.0 | Apache-2.0 OR MIT |
| [`once_cell`](https://crates.io/crates/once_cell) | 1.21.4 | MIT OR Apache-2.0 |
| [`pin-project-lite`](https://crates.io/crates/pin-project-lite) | 0.2.17 | Apache-2.0 OR MIT |
| [`ppv-lite86`](https://crates.io/crates/ppv-lite86) | 0.2.21 | MIT OR Apache-2.0 |
| [`proc-macro2`](https://crates.io/crates/proc-macro2) | 1.0.107 | MIT OR Apache-2.0 |
| [`quote`](https://crates.io/crates/quote) | 1.0.47 | MIT OR Apache-2.0 |
| [`rand`](https://crates.io/crates/rand) | 0.8.8 | MIT OR Apache-2.0 |
| [`rand_chacha`](https://crates.io/crates/rand_chacha) | 0.3.1 | MIT OR Apache-2.0 |
| [`rand_core`](https://crates.io/crates/rand_core) | 0.6.4 | MIT OR Apache-2.0 |
| [`rustls`](https://crates.io/crates/rustls) | 0.23.46 | Apache-2.0 OR ISC OR MIT |
| [`rustls-pki-types`](https://crates.io/crates/rustls-pki-types) | 1.16.0 | MIT OR Apache-2.0 |
| [`rustls-platform-verifier`](https://crates.io/crates/rustls-platform-verifier) | 0.5.3 | MIT OR Apache-2.0 |
| [`rustls-webpki`](https://crates.io/crates/rustls-webpki) | 0.103.17 | ISC |
| [`serde`](https://crates.io/crates/serde) | 1.0.229 | MIT OR Apache-2.0 |
| [`serde_core`](https://crates.io/crates/serde_core) | 1.0.229 | MIT OR Apache-2.0 |
| [`serde_derive`](https://crates.io/crates/serde_derive) | 1.0.229 (proc-macro) | MIT OR Apache-2.0 |
| [`serde_json`](https://crates.io/crates/serde_json) | 1.0.154 | MIT OR Apache-2.0 |
| [`socket2`](https://crates.io/crates/socket2) | 0.6.5 | MIT OR Apache-2.0 |
| [`subtle`](https://crates.io/crates/subtle) | 2.6.1 | BSD-3-Clause |
| [`syn`](https://crates.io/crates/syn) | 2.0.119 | MIT OR Apache-2.0 |
| [`syn`](https://crates.io/crates/syn) | 3.0.10 | MIT OR Apache-2.0 |
| [`tokio`](https://crates.io/crates/tokio) | 1.54.1 | MIT |
| [`tokio-macros`](https://crates.io/crates/tokio-macros) | 2.7.2 (proc-macro) | MIT |
| [`tokio-rustls`](https://crates.io/crates/tokio-rustls) | 0.26.6 | MIT OR Apache-2.0 |
| [`tracing`](https://crates.io/crates/tracing) | 0.1.44 | MIT |
| [`tracing-attributes`](https://crates.io/crates/tracing-attributes) | 0.1.31 (proc-macro) | MIT |
| [`tracing-core`](https://crates.io/crates/tracing-core) | 0.1.36 | MIT |
| [`typenum`](https://crates.io/crates/typenum) | 1.20.1 | MIT OR Apache-2.0 |
| [`unicode-ident`](https://crates.io/crates/unicode-ident) | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| [`untrusted`](https://crates.io/crates/untrusted) | 0.9.0 | ISC |
| [`windows-link`](https://crates.io/crates/windows-link) | 0.2.1 | MIT OR Apache-2.0 |
| [`windows-sys`](https://crates.io/crates/windows-sys) | 0.59.0 | MIT OR Apache-2.0 |
| [`windows-sys`](https://crates.io/crates/windows-sys) | 0.61.2 | MIT OR Apache-2.0 |
| [`windows-targets`](https://crates.io/crates/windows-targets) | 0.52.6 | MIT OR Apache-2.0 |
| [`windows_x86_64_msvc`](https://crates.io/crates/windows_x86_64_msvc) | 0.52.6 | MIT OR Apache-2.0 |
| [`zerocopy`](https://crates.io/crates/zerocopy) | 0.8.63 | BSD-2-Clause OR Apache-2.0 OR MIT |
| [`zeroize`](https://crates.io/crates/zeroize) | 1.9.1 | Apache-2.0 OR MIT |
| [`zmij`](https://crates.io/crates/zmij) | 1.0.23 | MIT |
