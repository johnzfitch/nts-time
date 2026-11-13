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

use std::collections::{BTreeSet, HashMap, HashSet};
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

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Config, String> {
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
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        let mut val = || args.next().ok_or_else(|| format!("missing value for {a}"));
        match a.as_str() {
            "--apply" => c.apply = true,
            "--servers" => {
                c.servers = val()?
                    .split(',')
                    .map(|s| match s.split_once('=') {
                        Some((h, g)) => {
                            (h.trim().to_ascii_lowercase(), g.trim().to_ascii_lowercase())
                        }
                        None => (s.trim().to_ascii_lowercase(), s.trim().to_ascii_lowercase()),
                    })
                    .collect()
            }
            "--samples" => c.samples = number(&val()?, &a)?,
            "--min-agree" => c.min_agree = number(&val()?, &a)?,
            "--max-offset" => c.max_offset = number(&val()?, &a)?,
            "--step-threshold" => c.step_threshold = number(&val()?, &a)?,
            "--max-slew-ppm" => c.max_slew_ppm = number(&val()?, &a)?,
            "--log" => c.log = Some(val()?),
            _ => return Err(format!("unknown or combined diagnostic option: {a}")),
        }
    }
    if !(1..=8).contains(&c.samples) {
        return Err("--samples must be between 1 and 8".into());
    }
    if !c.max_offset.is_finite() || c.max_offset < 0.0 {
        return Err("--max-offset must be finite and nonnegative".into());
    }
    clock::validate_settings(c.step_threshold, c.max_slew_ppm)?;
    let mut hosts = HashSet::new();
    let mut groups = HashSet::new();
    for (host, group) in &mut c.servers {
        *host = host.strip_suffix('.').unwrap_or(host).to_string();
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            *host = ip.to_string();
        } else if host.is_empty()
            || host.len() > 253
            || !host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label.as_bytes()[0].is_ascii_alphanumeric()
                    && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err(format!("invalid server hostname: {host:?}"));
        }
        if group.is_empty()
            || !group
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
        {
            return Err(format!("invalid operator group: {group:?}"));
        }
        if !hosts.insert(host.clone()) {
            return Err(format!("duplicate server hostname: {host}"));
        }
        groups.insert(group.clone());
    }
    if c.min_agree == 0 || c.min_agree > groups.len() {
        return Err("--min-agree must be between 1 and the number of configured operators".into());
    }
    Ok(c)
}

fn number<T: std::str::FromStr>(value: &str, flag: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("invalid value for {flag}: {value:?}"))
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

fn to_ntp(t: SystemTime) -> Result<(NtpTimestamp, u128), String> {
    let d = t
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock before 1970")?;
    let secs = (d.as_secs() + NTP_UNIX_OFFSET) as u32; // era wrap is handled by NtpTimestamp subtraction
    Ok((
        NtpTimestamp::from_seconds_nanos_since_ntp_era(secs, d.subsec_nanos()),
        d.as_nanos(),
    ))
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
    let tcp = timeout(
        Duration::from_secs(5),
        TcpStream::connect((r.host.as_str(), 4460)),
    )
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

    let addr = timeout(
        Duration::from_secs(5),
        lookup_host((res.remote.as_str(), res.port)),
    )
    .await
    .map_err(|_| format!("resolve {}: timed out", res.remote))?
    .map_err(|e| format!("resolve {}: {e}", res.remote))?
    .next()
    .ok_or_else(|| format!("resolve {}: no address", res.remote))?;
    r.ntp_server = Some(format!("{}:{} ({addr})", res.remote, res.port));
    let bind = if addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let sock = UdpSocket::bind(bind)
        .await
        .map_err(|e| format!("bind: {e}"))?;
    sock.connect(addr)
        .await
        .map_err(|e| format!("connect udp: {e}"))?;

    for (i, cookie) in cookies.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        match one_sample(&sock, cookie, &c2s, &s2c, Duration::from_secs(3)).await {
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
    response_timeout: Duration,
) -> Result<Sample, String> {
    // Ask for one replacement cookie; we do not keep them between runs anyway.
    let (packet, id) = NtpPacket::nts_poll_message(cookie, 1, PollInterval::from_byte(6));
    let mut buf = [0u8; 1024];
    let mut cur = Cursor::new(&mut buf[..]);
    packet
        .serialize(&mut cur, c2s, None)
        .map_err(|e| format!("serialize: {e}"))?;
    let len = cur.position() as usize;

    // Bound draining: a peer continuously sending datagrams must not prevent the request.
    // Request identifiers below reject any stale packets left in the queue.
    let mut rbuf = [0u8; 2048];
    for _ in 0..32 {
        if sock.try_recv(&mut rbuf).is_err() {
            break;
        }
    }

    let t1 = SystemTime::now();
    let deadline = Instant::now() + response_timeout;
    timeout(response_timeout, sock.send(&buf[..len]))
        .await
        .map_err(|_| "send timed out".to_string())?
        .map_err(|e| format!("send: {e}"))?;
    let mut last_rejection = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        // timeout() alone is insufficient when recv() is continuously ready.
        if left.is_zero() {
            return Err(format!(
                "no valid response before deadline; last rejection: {last_rejection:?}"
            ));
        }
        let received = timeout(left, sock.recv(&mut rbuf)).await.map_err(|_| {
            format!("no valid response before deadline; last rejection: {last_rejection:?}")
        })?;
        let n = match received {
            Ok(n) => n,
            // Windows reports WSAEMSGSIZE instead of returning a truncated UDP datagram.
            Err(e) if e.raw_os_error() == Some(10040) => {
                last_rejection = Some("oversized datagram".into());
                continue;
            }
            Err(e) => return Err(format!("recv: {e}")),
        };
        let t4 = SystemTime::now();

        // Fails if the NTS authenticator does not decrypt with our server-to-client key.
        let resp = match NtpPacket::deserialize(&rbuf[..n], s2c) {
            Ok((p, _)) => p,
            Err(e) => {
                last_rejection = Some(format!("rejected packet: {e}"));
                continue;
            }
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
        let (t1n, t1_unix_ns) = to_ntp(t1)?;
        let (t4n, _) = to_ntp(t4)?;
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
    let mut edges = Vec::new();
    for (index, &(_, _, o, l)) in good.iter().enumerate() {
        edges.push((o - l, true, index));
        edges.push((o + l, false, index));
    }
    // starts before ends at equal positions so touching intervals count as overlapping
    edges.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut active = BTreeSet::new();
    let mut group_counts = HashMap::new();
    let mut best = (false, 0, 0);
    let mut chosen = BTreeSet::new();
    for &(_, start, index) in &edges {
        let group = good[index].1;
        if start {
            active.insert(index);
            *group_counts.entry(group).or_insert(0usize) += 1;
        } else {
            active.remove(&index);
            let count = group_counts.get_mut(group).unwrap();
            *count -= 1;
            if *count == 0 {
                group_counts.remove(group);
            }
        }
        let eligible = group_counts.len() >= min_agree && active.len() * 2 > good.len();
        // An ineligible cluster must never hide a smaller qualifying overlap.
        let score = (eligible, active.len(), group_counts.len());
        if score > best {
            best = score;
            chosen = active.clone();
        }
    }
    let chimers: Vec<_> = chosen.iter().map(|&index| &good[index]).collect();
    let mut groups: Vec<&str> = chimers.iter().map(|c| c.1).collect();
    groups.sort_unstable();
    groups.dedup();
    let mut offs: Vec<f64> = chimers.iter().map(|c| c.2).collect();
    offs.sort_by(f64::total_cmp);
    let median = (!offs.is_empty()).then(|| {
        let m = offs.len() / 2;
        let median = if offs.len() % 2 == 1 {
            offs[m]
        } else {
            (offs[m - 1] + offs[m]) / 2.0
        };
        let low = chimers
            .iter()
            .map(|c| c.2 - c.3)
            .fold(f64::NEG_INFINITY, f64::max);
        let high = chimers
            .iter()
            .map(|c| c.2 + c.3)
            .fold(f64::INFINITY, f64::min);
        // Agreement constrains the correction itself, not just which servers vote.
        median.clamp(low, high)
    });
    let ok = groups.len() >= min_agree && chimers.len() * 2 > good.len();
    Selection {
        truechimers: chimers.iter().map(|c| c.0.to_string()).collect(),
        groups_agreeing: groups.len(),
        offset: if ok { median } else { None },
        reason: if ok {
            format!(
                "{} of {} responding servers agree",
                chimers.len(),
                good.len()
            )
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && matches!(args[0].as_str(), "--version" | "-V") {
        println!("nts-time {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.as_slice() == ["--slew-test"] {
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
    let cfg = parse_args(args).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        usage()
    });
    let started = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(_) => {
            eprintln!("error: system clock before 1970; set the approximate date first");
            std::process::exit(1);
        }
    };

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
            format!(
                "refused: offset {o:.6} s exceeds --max-offset {}",
                cfg.max_offset
            ),
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
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
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

    #[test]
    fn correction_stays_inside_every_selected_interval() {
        let r = [
            res("near", "a", Some(0.0), 0.0),
            res("far1", "b", Some(0.9), 2.0),
            res("far2", "c", Some(1.0), 2.0),
        ];
        let selection = select(&r, 3);
        let offset = selection.offset.expect("three operators overlap");
        for server in &r {
            let sample = server.best.as_ref().unwrap();
            let radius = sample.delay / 2.0 + 0.002;
            assert!((sample.offset - radius..=sample.offset + radius).contains(&offset));
        }
    }

    #[test]
    fn finds_eligible_overlap_instead_of_same_operator_cluster() {
        let r = [
            res("a1", "a", Some(0.0), 0.0),
            res("a2", "a", Some(0.0), 0.0),
            res("a3", "a", Some(0.0), 0.0),
            res("bridge", "a", Some(0.5), 1.0),
            res("b", "b", Some(1.0), 0.0),
            res("c", "c", Some(1.0), 0.0),
            res("d", "d", Some(1.0), 0.0),
        ];
        let selection = select(&r, 3);
        assert!(selection.offset.is_some());
        assert_eq!(selection.groups_agreeing, 4);
    }

    #[tokio::test]
    async fn malformed_datagram_does_not_discard_later_authenticated_reply() {
        let keys = ntp_proto::test_cookie();
        let c2s: Option<&dyn Cipher> = Some(keys.c2s.as_ref());
        let s2c: Option<&dyn Cipher> = Some(keys.s2c.as_ref());
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(server.local_addr().unwrap()).await.unwrap();
        let reply = async {
            let mut buf = [0u8; 2048];
            let (n, peer) = server.recv_from(&mut buf).await.unwrap();
            let (mut packet, _) = NtpPacket::deserialize(&buf[..n], &c2s).unwrap();
            packet.set_mode(NtpAssociationMode::Server);
            packet.set_stratum(1);
            packet.set_leap(NtpLeapIndicator::NoWarning);
            packet.set_origin_timestamp(packet.transmit_timestamp());
            let now = to_ntp(SystemTime::now()).unwrap().0;
            packet.set_receive_timestamp(now);
            packet.set_transmit_timestamp(now);
            let mut response_buf = [0u8; 2048];
            let mut response = Cursor::new(&mut response_buf[..]);
            packet.serialize(&mut response, &s2c, None).unwrap();
            let len = response.position() as usize;
            server.send_to(b"not an NTP packet", peer).await.unwrap();
            server.send_to(&[0; 4096], peer).await.unwrap();
            let mut tampered = response_buf[..len].to_vec();
            // Header modification invalidates the NTS tag; the matching identifier alone is insufficient.
            tampered[1] = 2;
            server.send_to(&tampered, peer).await.unwrap();
            // Even a valid authenticator must echo this request's origin timestamp.
            packet.set_origin_timestamp(NtpTimestamp::from_seconds_nanos_since_ntp_era(0, 0));
            packet.set_stratum(3);
            let mut wrong_origin_buf = [0u8; 2048];
            let mut wrong_origin = Cursor::new(&mut wrong_origin_buf[..]);
            packet.serialize(&mut wrong_origin, &s2c, None).unwrap();
            let wrong_len = wrong_origin.position() as usize;
            server
                .send_to(&wrong_origin_buf[..wrong_len], peer)
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
            server.send_to(&response_buf[..len], peer).await.unwrap();
        };
        let (sample, ()) = tokio::join!(
            one_sample(&client, &[0; 32], &c2s, &s2c, Duration::from_secs(3)),
            reply
        );
        assert_eq!(sample.unwrap().stratum, 1);
    }

    #[test]
    fn numeric_options_reject_nonfinite_and_unsafe_values() {
        for flag in ["--max-offset", "--step-threshold", "--max-slew-ppm"] {
            for value in ["NaN", "inf", "-inf", "1e999", "-1"] {
                assert!(
                    parse_args([flag.to_string(), value.to_string()]).is_err(),
                    "{flag} {value}"
                );
            }
        }
        for value in ["0", "500.001", "1000000"] {
            assert!(parse_args(["--max-slew-ppm".into(), value.into()]).is_err());
        }
        assert!(
            parse_args([
                "--max-offset".into(),
                "0".into(),
                "--step-threshold".into(),
                "0".into()
            ])
            .is_ok()
        );
        assert!(parse_args(["--max-slew-ppm".into(), "0.001".into()]).is_ok());
    }

    #[test]
    fn invalid_server_lists_and_quorums_are_rejected() {
        for servers in [
            "",
            ",",
            "a=",
            "=a",
            "a=x,,b=y",
            "a=x,",
            "a=x,a=y",
            "EXAMPLE.com=x,example.com.=y",
            "a...=x",
            "a=foo bar",
            "a\"=x",
            "a=x\" --log file",
        ] {
            assert!(
                parse_args([
                    "--min-agree".into(),
                    "1".into(),
                    "--servers".into(),
                    servers.into()
                ])
                .is_err(),
                "{servers:?}"
            );
        }
        assert!(parse_args(["--servers".into(), "a=x,b=x,c=y".into()]).is_err());
        for value in ["0", "6"] {
            assert!(parse_args(["--min-agree".into(), value.into()]).is_err());
        }
        for value in ["0", "9"] {
            assert!(parse_args(["--samples".into(), value.into()]).is_err());
        }
    }

    #[test]
    fn server_names_and_operator_groups_are_normalized() {
        let c = parse_args(["--servers".into(), "A.Example.=ONE, b=Two,::1=THREE".into()]).unwrap();
        assert_eq!(
            c.servers,
            vec![
                ("a.example".into(), "one".into()),
                ("b".into(), "two".into()),
                ("::1".into(), "three".into())
            ]
        );
        assert!(parse_args(["--servers".into(), "a=X,b=x,c=y".into()]).is_err());
    }

    #[test]
    fn diagnostic_options_cannot_be_combined_with_normal_run_flags() {
        assert!(parse_args(["--slew-test".into(), "--version".into()]).is_err());
        assert!(parse_args(["--apply".into(), "--slew-test".into()]).is_err());
        assert!(parse_args(["--samples".into()]).is_err());
    }

    #[test]
    fn ntp_conversion_handles_era_wrap_and_rejects_pre_epoch_time() {
        assert!(to_ntp(UNIX_EPOCH - Duration::from_secs(1)).is_err());
        let wrap = UNIX_EPOCH + Duration::from_secs((1u64 << 32) - NTP_UNIX_OFFSET);
        let (before, _) = to_ntp(wrap - Duration::from_secs(1)).unwrap();
        let (after, _) = to_ntp(wrap + Duration::from_secs(1)).unwrap();
        // ntp-proto's fixed-point conversion is accurate to one NTP fractional unit.
        assert!(((after - before).to_seconds() - 2.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn invalid_traffic_cannot_extend_the_response_deadline() {
        let keys = ntp_proto::test_cookie();
        let c2s: Option<&dyn Cipher> = Some(keys.c2s.as_ref());
        let s2c: Option<&dyn Cipher> = Some(keys.s2c.as_ref());
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(server.local_addr().unwrap()).await.unwrap();
        let peer = client.local_addr().unwrap();
        let flood = async {
            loop {
                server.send_to(b"bad packet", peer).await.unwrap();
                tokio::task::yield_now().await;
            }
        };
        let sample = one_sample(&client, &[0; 32], &c2s, &s2c, Duration::from_millis(20));
        let result = timeout(Duration::from_secs(1), async {
            tokio::select! {
                result = sample => result,
                () = flood => unreachable!(),
            }
        })
        .await
        .expect("invalid traffic kept the request alive");
        assert!(result.err().unwrap().contains("deadline"));
    }
}
