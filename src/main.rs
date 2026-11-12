//! nts-time: one-shot Windows clock synchronisation over NTS (RFC 8915).
//!
//! Each run does an NTS key exchange (TLS 1.3, certificate checked against the
//! Windows certificate store) with every configured server, sends a few
//! NTS-authenticated NTPv4 requests to each, keeps the lowest-delay sample per
//! server, intersects the servers' error intervals, and only touches the clock
//! when enough independent operators agree. Without `--apply` it only reports.
//!
//! Protocol code (NTS-KE, cookies, AEAD_AES_SIV_CMAC, packet parsing and
//! response validation) comes from `ntp-proto`, the library behind ntpd-rs.

mod clock;

use std::io::Cursor;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ntp_proto::{
    Cipher, KeyExchangeClient, NtpAssociationMode, NtpLeapIndicator, NtpPacket, NtpTimestamp,
    NtpVersion, NtsClientConfig, PollInterval,
};
use serde::Serialize;
use tokio::net::{TcpStream, UdpSocket, lookup_host};
use tokio::time::timeout;

const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

/// Default servers, one per independent operator ("group").
const DEFAULT_SERVERS: &[(&str, &str)] = &[
    ("time.cloudflare.com", "cloudflare"),
    ("nts.netnod.se", "netnod"),
    ("ptbtime1.ptb.de", "ptb"),
    ("nts.time.nl", "sidn"),
    ("virginia.time.system76.com", "system76"),
];

struct Config {
    apply: bool,
    servers: Vec<(String, String)>,
    samples: usize,
    min_agree: usize,
    max_offset: f64,
    step_threshold: f64,
    max_slew_ppm: f64,
    log: Option<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: nts-time [--apply] [--version] [--slew-test]\n\
         \x20               [--servers host=group,host=group,...] [--samples N]\n\
         \x20               [--min-agree N] [--max-offset SECS] [--step-threshold SECS]\n\
         \x20               [--max-slew-ppm PPM] [--log FILE.jsonl]\n\
         Without --apply nothing on the clock is changed."
    );
    std::process::exit(64)
}

fn parse_args() -> Config {
    let mut c = Config {
        apply: false,
        servers: DEFAULT_SERVERS
            .iter()
            .map(|(h, g)| (h.to_string(), g.to_string()))
            .collect(),
        samples: 4,
        min_agree: 3,
        max_offset: 86_400.0,
        step_threshold: 0.128,
        max_slew_ppm: 500.0,
        log: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--apply" => c.apply = true,
            "--version" | "-V" => {
                println!("nts-time {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0)
            }
            "--servers" => {
                c.servers = val()
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| match s.split_once('=') {
                        Some((h, g)) => (h.to_string(), g.to_string()),
                        None => (s.to_string(), s.to_string()),
                    })
                    .collect()
            }
            "--samples" => c.samples = val().parse().unwrap_or_else(|_| usage()),
            "--min-agree" => c.min_agree = val().parse().unwrap_or_else(|_| usage()),
            "--max-offset" => c.max_offset = val().parse().unwrap_or_else(|_| usage()),
            "--step-threshold" => c.step_threshold = val().parse().unwrap_or_else(|_| usage()),
            "--max-slew-ppm" => c.max_slew_ppm = val().parse().unwrap_or_else(|_| usage()),
            "--log" => c.log = Some(val()),
            _ => usage(),
        }
    }
    if c.samples == 0 || c.samples > 8 || c.min_agree == 0 || c.max_slew_ppm <= 0.0 {
        usage();
    }
    c
}

#[derive(Serialize, Clone)]
struct Sample {
    /// Local send time, nanoseconds since the Unix epoch.
    t1_unix_ns: u128,
    /// Server receive / transmit times and local receive time, seconds after t1.
    t2_rel: f64,
    t3_rel: f64,
    t4_rel: f64,
    offset: f64,
    delay: f64,
    stratum: u8,
    root_delay: f64,
    root_dispersion: f64,
}

#[derive(Serialize)]
struct ServerResult {
    host: String,
    group: String,
    ke_ms: Option<f64>,
    ntp_server: Option<String>,
    samples: Vec<Sample>,
    sample_errors: Vec<String>,
    best: Option<Sample>,
    error: Option<String>,
}

fn to_ntp(t: SystemTime) -> (NtpTimestamp, u128) {
    let d = t.duration_since(UNIX_EPOCH).expect("clock before 1970");
    let secs = (d.as_secs() + NTP_UNIX_OFFSET) as u32; // era wrap is handled by NtpTimestamp subtraction
    (
        NtpTimestamp::from_seconds_nanos_since_ntp_era(secs, d.subsec_nanos()),
        d.as_nanos(),
    )
}

async fn query_server(ke: &KeyExchangeClient, host: &str, group: &str, n: usize) -> ServerResult {
    let mut r = ServerResult {
        host: host.into(),
        group: group.into(),
        ke_ms: None,
        ntp_server: None,
        samples: vec![],
        sample_errors: vec![],
        best: None,
        error: None,
    };
    if let Err(e) = query_inner(ke, &mut r, n).await {
        r.error = Some(e);
    }
    r.best = r
        .samples
        .iter()
        .min_by(|a, b| a.delay.total_cmp(&b.delay))
        .cloned();
    r
}

async fn query_inner(ke: &KeyExchangeClient, r: &mut ServerResult, n: usize) -> Result<(), String> {
    let ke_start = Instant::now();
    let tcp = timeout(Duration::from_secs(5), TcpStream::connect((r.host.as_str(), 4460)))
        .await
        .map_err(|_| "NTS-KE connect timed out".to_string())?
        .map_err(|e| format!("NTS-KE connect: {e}"))?;
    let res = timeout(
        Duration::from_secs(10),
        ke.exchange_keys(tcp, r.host.clone(), std::iter::empty()),
    )
    .await
    .map_err(|_| "NTS-KE timed out".to_string())?
    .map_err(|e| format!("NTS-KE: {e}"))?;
    r.ke_ms = Some(ke_start.elapsed().as_secs_f64() * 1e3);

    let mut nts = res.nts;
    // Each request consumes its own cookie; cookies are never reused.
    let cookies: Vec<Vec<u8>> = (0..n).filter_map(|_| nts.get_cookie()).collect();
    let (c2s, s2c) = (*nts).get_keys();
    let c2s: Option<&dyn Cipher> = Some(c2s.as_ref());
    let s2c: Option<&dyn Cipher> = Some(s2c.as_ref());

    let addr = lookup_host((res.remote.as_str(), res.port))
        .await
        .map_err(|e| format!("resolve {}: {e}", res.remote))?
        .next()
        .ok_or_else(|| format!("resolve {}: no address", res.remote))?;
    r.ntp_server = Some(format!("{}:{} ({addr})", res.remote, res.port));
    let bind = if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = UdpSocket::bind(bind).await.map_err(|e| format!("bind: {e}"))?;
    sock.connect(addr).await.map_err(|e| format!("connect udp: {e}"))?;

    for (i, cookie) in cookies.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        match one_sample(&sock, cookie, &c2s, &s2c).await {
            Ok(s) => r.samples.push(s),
            Err(e) => r.sample_errors.push(e),
        }
    }
    if r.samples.is_empty() {
        return Err("no valid authenticated responses".into());
    }
    Ok(())
}

async fn one_sample(
    sock: &UdpSocket,
    cookie: &[u8],
    c2s: &Option<&dyn Cipher>,
    s2c: &Option<&dyn Cipher>,
) -> Result<Sample, String> {
    // Ask for one replacement cookie; we do not keep them between runs anyway.
    let (packet, id) = NtpPacket::nts_poll_message(cookie, 1, PollInterval::from_byte(6));
    let mut buf = [0u8; 1024];
    let mut cur = Cursor::new(&mut buf[..]);
    packet
        .serialize(&mut cur, c2s, None)
        .map_err(|e| format!("serialize: {e}"))?;
    let len = cur.position() as usize;

    // Drain anything stale so a late reply to an earlier request is not mistaken for this one.
    let mut rbuf = [0u8; 2048];
    while sock.try_recv(&mut rbuf).is_ok() {}

    let t1 = SystemTime::now();
    sock.send(&buf[..len]).await.map_err(|e| format!("send: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let n = timeout(left, sock.recv(&mut rbuf))
            .await
            .map_err(|_| "no response within 3 s".to_string())?
            .map_err(|e| format!("recv: {e}"))?;
        let t4 = SystemTime::now();

        // Fails if the NTS authenticator does not decrypt with our server-to-client key.
        let resp = match NtpPacket::deserialize(&rbuf[..n], s2c) {
            Ok((p, _)) => p,
            Err(e) => return Err(format!("rejected packet: {e}")),
        };
        // Requires our unique identifier inside the authenticated part and our origin timestamp.
        if !resp.valid_server_response(id, true) {
            continue; // not a reply to this request; keep waiting until the deadline
        }
        if resp.is_kiss() {
            return Err(format!("kiss-o'-death {:?}", resp.reference_id()));
        }
        if resp.version() != NtpVersion::V4 || resp.mode() != NtpAssociationMode::Server {
            return Err("unexpected version or mode".into());
        }
        if matches!(
            resp.leap(),
            NtpLeapIndicator::Unknown | NtpLeapIndicator::Unsynchronized
        ) {
            return Err("server reports itself unsynchronised".into());
        }
        let stratum = resp.stratum();
        if !(1..=15).contains(&stratum) {
            return Err(format!("bad stratum {stratum}"));
        }
        let (t1n, t1_unix_ns) = to_ntp(t1);
        let (t4n, _) = to_ntp(t4);
        let t2 = resp.receive_timestamp();
        let t3 = resp.transmit_timestamp();
        let t2_rel = (t2 - t1n).to_seconds();
        let t3_rel = (t3 - t1n).to_seconds();
        let t4_rel = (t4n - t1n).to_seconds();
        let offset = (t2_rel + (t3_rel - t4_rel)) / 2.0;
        let delay = t4_rel - (t3_rel - t2_rel);
        let root_delay = resp.root_delay().to_seconds();
        let root_dispersion = resp.root_dispersion().to_seconds();
        if !(0.0..=2.0).contains(&delay) || t3_rel < t2_rel {
            return Err(format!("implausible timestamps (delay {delay:.6} s)"));
        }
        if root_delay / 2.0 + root_dispersion > 1.0 {
            return Err("server root distance above 1 s".into());
        }
        return Ok(Sample {
            t1_unix_ns,
            t2_rel,
            t3_rel,
            t4_rel,
            offset,
            delay,
            stratum,
            root_delay,
            root_dispersion,
        });
    }
}

#[derive(Serialize)]
struct Selection {
    /// Hosts whose error intervals contain the agreed point.
    truechimers: Vec<String>,
    groups_agreeing: usize,
    offset: Option<f64>,
    reason: String,
}

/// Marzullo-style intersection of [offset - λ, offset + λ] intervals,
/// λ = delay/2 + root_delay/2 + root_dispersion + 2 ms floor for local jitter.
fn select(results: &[ServerResult], min_agree: usize) -> Selection {
    let good: Vec<(&str, &str, f64, f64)> = results
        .iter()
        .filter_map(|r| {
            r.best.as_ref().map(|s| {
                let lambda = s.delay / 2.0 + s.root_delay / 2.0 + s.root_dispersion + 0.002;
                (r.host.as_str(), r.group.as_str(), s.offset, lambda)
            })
        })
        .collect();
    let mut edges: Vec<(f64, i32)> = Vec::new();
    for &(_, _, o, l) in &good {
        edges.push((o - l, 1));
        edges.push((o + l, -1));
    }
    // starts before ends at equal positions so touching intervals count as overlapping
    edges.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)));
    let (mut best, mut cur, mut point) = (0, 0, 0.0);
    for &(x, d) in &edges {
        cur += d;
        if cur > best {
            best = cur;
            point = x;
        }
    }
    let chimers: Vec<_> = good
        .iter()
        .filter(|&&(_, _, o, l)| (o - l..=o + l).contains(&point))
        .collect();
    let mut groups: Vec<&str> = chimers.iter().map(|c| c.1).collect();
    groups.sort_unstable();
    groups.dedup();
    let mut offs: Vec<f64> = chimers.iter().map(|c| c.2).collect();
    offs.sort_by(f64::total_cmp);
    let median = (!offs.is_empty()).then(|| {
        let m = offs.len() / 2;
        if offs.len() % 2 == 1 { offs[m] } else { (offs[m - 1] + offs[m]) / 2.0 }
    });
    let ok = groups.len() >= min_agree && chimers.len() * 2 > good.len();
    Selection {
        truechimers: chimers.iter().map(|c| c.0.to_string()).collect(),
        groups_agreeing: groups.len(),
        offset: if ok { median } else { None },
        reason: if ok {
            format!("{} of {} responding servers agree", chimers.len(), good.len())
        } else {
            format!(
                "not enough agreement: {} operator(s) agree, {} responded, need {} operators and a majority",
                groups.len(),
                good.len(),
                min_agree
            )
        },
    }
}

#[derive(Serialize)]
struct Run {
    version: &'static str,
    started_unix: f64,
    apply: bool,
    servers: Vec<ServerResult>,
    selection: Selection,
    action: String,
    action_detail: Option<clock::ActionReport>,
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--slew-test") {
        match clock::slew_test(10.0, 500.0) {
            Ok(t) => {
                println!(
                    "time increment {} (100 ns units), initial adjustment {}, initially disabled {}",
                    t.time_increment_100ns, t.initial_adjustment_100ns, t.initially_disabled
                );
                for p in &t.phases {
                    println!(
                        "{:+7.1} ppm (adj {}): drift over phase {:+.6} s, inside phase {:+.6} s, expected {:+.6} s",
                        p.ppm,
                        p.adjustment_100ns,
                        p.drift_total,
                        p.drift_inner,
                        p.ppm * 1e-6 * 10.0
                    );
                }
                let dir = std::path::PathBuf::from(
                    std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into()),
                )
                .join("nts-time");
                let _ = std::fs::create_dir_all(&dir);
                let path = dir.join("slew-test.json");
                let _ = std::fs::write(&path, serde_json::to_string(&t).unwrap());
                let path = path.display();
                println!("raw samples: {path}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
    let cfg = parse_args();
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    if cfg.apply {
        // Undo any slew a previous run left behind if it was killed mid-correction.
        if let Err(e) = clock::prepare() {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let ke = KeyExchangeClient::new(&NtsClientConfig::default()).expect("NTS-KE client");
    let servers: Vec<ServerResult> = rt.block_on(async {
        let mut out = Vec::new();
        for (h, g) in &cfg.servers {
            out.push(query_server(&ke, h, g, cfg.samples).await);
        }
        out
    });

    let selection = select(&servers, cfg.min_agree);
    let (action, detail, code) = match selection.offset {
        None => ("refused: no agreement".to_string(), None, 2),
        Some(o) if o.abs() > cfg.max_offset => (
            format!("refused: offset {o:.6} s exceeds --max-offset {}", cfg.max_offset),
            None,
            3,
        ),
        Some(o) if !cfg.apply => (format!("dry run: would correct {o:+.6} s"), None, 0),
        Some(o) => match clock::correct(o, cfg.step_threshold, cfg.max_slew_ppm) {
            Ok(rep) => (rep.summary(), Some(rep), 0),
            Err(e) => (format!("failed: {e}"), None, 1),
        },
    };

    let run = Run {
        version: env!("CARGO_PKG_VERSION"),
        started_unix: started,
        apply: cfg.apply,
        servers,
        selection,
        action,
        action_detail: detail,
    };

    for s in &run.servers {
        match (&s.best, &s.error) {
            (Some(b), _) => println!(
                "{:<28} offset {:+.6} s  delay {:.6} s  stratum {}  ({} of {} samples ok)",
                s.host,
                b.offset,
                b.delay,
                b.stratum,
                s.samples.len(),
                s.samples.len() + s.sample_errors.len()
            ),
            (None, Some(e)) => println!("{:<28} FAILED: {e}", s.host),
            (None, None) => println!("{:<28} FAILED", s.host),
        }
    }
    println!(
        "agreeing: {} -> {}",
        run.selection.truechimers.join(", "),
        run.selection.reason
    );
    println!("{}", run.action);

    let line = serde_json::to_string(&run).expect("serialize run");
    if let Some(path) = &cfg.log {
        use std::io::Write;
        if let Some(dir) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(mut f) => {
                let _ = writeln!(f, "{line}");
            }
            Err(e) => eprintln!("warning: cannot write log {path}: {e}"),
        }
    }
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(host: &str, group: &str, offset: Option<f64>, delay: f64) -> ServerResult {
        ServerResult {
            host: host.into(),
            group: group.into(),
            ke_ms: None,
            ntp_server: None,
            samples: vec![],
            sample_errors: vec![],
            best: offset.map(|offset| Sample {
                t1_unix_ns: 0,
                t2_rel: 0.0,
                t3_rel: 0.0,
                t4_rel: 0.0,
                offset,
                delay,
                stratum: 1,
                root_delay: 0.0,
                root_dispersion: 0.0,
            }),
            error: None,
        }
    }

    #[test]
    fn all_agree_gives_median() {
        let r = [
            res("a", "a", Some(-0.844), 0.005),
            res("b", "b", Some(-0.848), 0.16),
            res("c", "c", Some(-0.843), 0.16),
            res("d", "d", Some(-0.8437), 0.14),
            res("e", "e", Some(-0.8434), 0.07),
        ];
        let s = select(&r, 3);
        assert_eq!(s.truechimers.len(), 5);
        assert_eq!(s.offset, Some(-0.8437));
    }

    #[test]
    fn one_liar_is_excluded() {
        let r = [
            res("a", "a", Some(0.010), 0.01),
            res("b", "b", Some(0.011), 0.01),
            res("c", "c", Some(0.009), 0.01),
            res("evil", "evil", Some(3600.0), 0.01),
        ];
        let s = select(&r, 3);
        assert!(!s.truechimers.contains(&"evil".to_string()));
        assert_eq!(s.offset, Some(0.010));
    }

    #[test]
    fn too_few_operators_refuses() {
        let r = [
            res("a", "a", Some(0.0), 0.01),
            res("b", "b", Some(0.0), 0.01),
            res("c", "c", None, 0.0),
        ];
        assert_eq!(select(&r, 3).offset, None);
    }

    #[test]
    fn same_operator_counts_once() {
        let r = [
            res("a1", "a", Some(0.0), 0.01),
            res("a2", "a", Some(0.0), 0.01),
            res("b", "b", Some(0.0), 0.01),
        ];
        let s = select(&r, 3);
        assert_eq!(s.groups_agreeing, 2);
        assert_eq!(s.offset, None);
    }

    #[test]
    fn split_without_majority_refuses() {
        let r = [
            res("a", "a", Some(0.0), 0.01),
            res("b", "b", Some(0.0), 0.01),
            res("c", "c", Some(0.0), 0.01),
            res("d", "d", Some(5.0), 0.01),
            res("e", "e", Some(5.0), 0.01),
            res("f", "f", Some(5.0), 0.01),
        ];
        assert_eq!(select(&r, 3).offset, None);
    }
}
