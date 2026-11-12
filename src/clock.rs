//! Windows clock correction: step with SetSystemTime, or slew with
//! SetSystemTimeAdjustmentPrecise for a bounded time and then hand the clock
//! back to its nominal rate. Needs SeSystemtimePrivilege (SYSTEM or admin).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use windows_sys::Win32::Foundation::{
    CloseHandle, FALSE, FILETIME, GetLastError, HANDLE, LUID, SYSTEMTIME, TRUE,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::System::SystemInformation::{
    GetSystemTimeAdjustmentPrecise, SetSystemTime, SetSystemTimeAdjustmentPrecise,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::Time::FileTimeToSystemTime;

const UNIX_TO_FILETIME_100NS: i128 = 116_444_736_000_000_000;

#[derive(Serialize)]
pub struct ActionReport {
    pub kind: &'static str,
    pub requested: f64,
    /// Correction measured on the clock: system-time elapsed minus QPC elapsed.
    pub achieved: f64,
    pub slew_ppm: Option<f64>,
    pub slew_seconds: Option<f64>,
    pub time_increment_100ns: Option<u64>,
}

impl ActionReport {
    pub fn summary(&self) -> String {
        match self.kind {
            "step" => format!(
                "stepped clock by {:+.6} s (measured {:+.6} s)",
                self.requested, self.achieved
            ),
            "slew-failed-then-step" => format!(
                "WARNING slew misbehaved; stepped the remainder: total {:+.6} s requested, {:+.6} s measured",
                self.requested, self.achieved
            ),
            _ => format!(
                "slewed clock by {:+.6} s over {:.1} s at {:.1} ppm (measured {:+.6} s)",
                self.requested,
                self.slew_seconds.unwrap_or(0.0),
                self.slew_ppm.unwrap_or(0.0),
                self.achieved
            ),
        }
    }
}

fn last_error(what: &str) -> String {
    format!("{what} failed (Win32 error {})", unsafe { GetLastError() })
}

fn enable_systemtime_privilege() -> Result<(), String> {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == FALSE
        {
            return Err(last_error("OpenProcessToken"));
        }
        let name: Vec<u16> = "SeSystemtimePrivilege\0".encode_utf16().collect();
        let mut luid = LUID { LowPart: 0, HighPart: 0 };
        if LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) == FALSE {
            CloseHandle(token);
            return Err(last_error("LookupPrivilegeValueW"));
        }
        let mut tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [windows_sys::Win32::Security::LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let ok = AdjustTokenPrivileges(
            token,
            FALSE,
            &mut tp,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        // AdjustTokenPrivileges "succeeds" with ERROR_NOT_ALL_ASSIGNED (1300) when the privilege is absent.
        let err = GetLastError();
        CloseHandle(token);
        if ok == FALSE || err == 1300 {
            return Err(
                "SeSystemtimePrivilege not held: run as SYSTEM or from an elevated prompt".into(),
            );
        }
    }
    Ok(())
}

/// Puts the clock back on its nominal rate (undoing any leftover slew).
fn reset_adjustment() -> Result<(), String> {
    if unsafe { SetSystemTimeAdjustmentPrecise(0, TRUE) } == FALSE {
        return Err(last_error("SetSystemTimeAdjustmentPrecise(reset)"));
    }
    Ok(())
}

pub fn prepare() -> Result<(), String> {
    enable_systemtime_privilege()?;
    reset_adjustment()
}

struct ResetOnDrop;
impl Drop for ResetOnDrop {
    fn drop(&mut self) {
        let _ = reset_adjustment();
    }
}

fn sys_secs(t: SystemTime) -> f64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    }
}

/// The Precise adjustment is a clock update *frequency* (Microsoft Learn: "adjusted clock update
/// frequency"); the nominal value equals lpTimeIncrement, here 10_000_000 = the 10 MHz QPC rate,
/// i.e. counter ticks per second of system time. A larger value lengthens each system second, so
/// the clock runs SLOWER. Measured with --slew-test on Windows 11 26200 (2026-11-12): inc+5000
/// gave -5.0 ms over 10 s, inc-5000 gave +5.0 ms. WINtp (github.com/lalakii/WINtp) uses the same
/// sign. Do not go by the legacy SetSystemTimeAdjustment "added per interrupt" wording.
fn adjustment_for(inc: u64, delta: u64, faster: bool) -> u64 {
    if faster { inc - delta } else { inc + delta }
}

pub fn correct(offset: f64, step_threshold: f64, max_slew_ppm: f64) -> Result<ActionReport, String> {
    if offset.abs() >= step_threshold {
        return step(offset);
    }
    let (mut adj, mut inc, mut disabled) = (0u64, 0u64, 0);
    if unsafe { GetSystemTimeAdjustmentPrecise(&mut adj, &mut inc, &mut disabled) } == FALSE {
        return Err(last_error("GetSystemTimeAdjustmentPrecise"));
    }
    let delta = (inc as f64 * max_slew_ppm * 1e-6).round() as u64;
    if delta == 0 || inc == 0 {
        return step(offset); // rate too fine to express: fall back to a step
    }
    let rate = delta as f64 / inc as f64;
    let seconds = offset.abs() / rate;
    // offset > 0 means the local clock is behind, so it must run fast.
    let new_adj = adjustment_for(inc, delta, offset > 0.0);

    let _guard = ResetOnDrop;
    let (s0, i0) = (SystemTime::now(), Instant::now());
    if unsafe { SetSystemTimeAdjustmentPrecise(new_adj, FALSE) } == FALSE {
        return Err(last_error("SetSystemTimeAdjustmentPrecise"));
    }
    std::thread::sleep(Duration::from_secs_f64(seconds));
    reset_adjustment()?;
    let (s1, i1) = (SystemTime::now(), Instant::now());
    let achieved = (sys_secs(s1) - sys_secs(s0)) - (i1 - i0).as_secs_f64();
    // If the slew did not do what was asked (wrong sign or far off), step the remainder
    // so a misbehaving rate API can never accumulate error.
    if (achieved - offset).abs() > 0.5 * offset.abs() + 50e-6 {
        let mut rep = step(offset - achieved)?;
        rep.kind = "slew-failed-then-step";
        rep.requested = offset;
        rep.achieved += achieved;
        rep.slew_ppm = Some(rate * 1e6);
        rep.slew_seconds = Some(seconds);
        rep.time_increment_100ns = Some(inc);
        return Ok(rep);
    }
    Ok(ActionReport {
        kind: "slew",
        requested: offset,
        achieved,
        slew_ppm: Some(rate * 1e6),
        slew_seconds: Some(seconds),
        time_increment_100ns: Some(inc),
    })
}

#[derive(Serialize)]
pub struct SlewTestPhase {
    pub ppm: f64,
    pub adjustment_100ns: u64,
    /// (QPC seconds since phase start, system-time minus QPC drift in seconds), sampled every 100 ms.
    pub samples: Vec<(f64, f64)>,
    /// Drift measured across the set/reset calls, as `correct` measures it.
    pub drift_total: f64,
    /// Drift between the first and last in-phase samples (excludes any jump at set/reset).
    pub drift_inner: f64,
}

#[derive(Serialize)]
pub struct SlewTest {
    pub initial_adjustment_100ns: u64,
    pub time_increment_100ns: u64,
    pub initially_disabled: bool,
    pub phases: Vec<SlewTestPhase>,
}

/// Diagnostic: run 0, +ppm, -ppm, 0 for `secs` each and measure how system time moves against QPC.
pub fn slew_test(secs: f64, ppm: f64) -> Result<SlewTest, String> {
    enable_systemtime_privilege()?;
    let (mut adj, mut inc, mut disabled) = (0u64, 0u64, 0);
    if unsafe { GetSystemTimeAdjustmentPrecise(&mut adj, &mut inc, &mut disabled) } == FALSE {
        return Err(last_error("GetSystemTimeAdjustmentPrecise"));
    }
    let _guard = ResetOnDrop;
    reset_adjustment()?;
    let delta = (inc as f64 * ppm * 1e-6).round() as u64;
    let mut phases = Vec::new();
    // sign +1 = clock should run fast (+ppm), via the same mapping `correct` uses.
    for (sign, label) in [(0i64, 0.0), (1, ppm), (-1, -ppm), (0, 0.0)] {
        let new_adj = if sign == 0 { inc } else { adjustment_for(inc, delta, sign > 0) };
        let (s0, i0) = (SystemTime::now(), Instant::now());
        if sign != 0 && unsafe { SetSystemTimeAdjustmentPrecise(new_adj, FALSE) } == FALSE {
            return Err(last_error("SetSystemTimeAdjustmentPrecise"));
        }
        let mut samples = Vec::new();
        while i0.elapsed().as_secs_f64() < secs {
            let (s, i) = (SystemTime::now(), Instant::now());
            let q = (i - i0).as_secs_f64();
            samples.push((q, (sys_secs(s) - sys_secs(s0)) - q));
            std::thread::sleep(Duration::from_millis(100));
        }
        reset_adjustment()?;
        let (s1, i1) = (SystemTime::now(), Instant::now());
        let drift_inner = match (samples.first(), samples.last()) {
            (Some(a), Some(b)) => b.1 - a.1,
            _ => 0.0,
        };
        phases.push(SlewTestPhase {
            ppm: label,
            adjustment_100ns: if sign == 0 { inc } else { new_adj },
            samples,
            drift_total: (sys_secs(s1) - sys_secs(s0)) - (i1 - i0).as_secs_f64(),
            drift_inner,
        });
    }
    Ok(SlewTest {
        initial_adjustment_100ns: adj,
        time_increment_100ns: inc,
        initially_disabled: disabled != 0,
        phases,
    })
}

fn step(offset: f64) -> Result<ActionReport, String> {
    let (s0, i0) = (SystemTime::now(), Instant::now());
    let now_100ns = match s0.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_nanos() / 100) as i128,
        Err(_) => return Err("system clock before 1970".into()),
    };
    // SetSystemTime has millisecond resolution; round to the nearest millisecond.
    let target = now_100ns + UNIX_TO_FILETIME_100NS + (offset * 1e7).round() as i128 + 5_000;
    let target = target - target.rem_euclid(10_000);
    let ft = FILETIME {
        dwLowDateTime: target as u32,
        dwHighDateTime: (target >> 32) as u32,
    };
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { FileTimeToSystemTime(&ft, &mut st) } == FALSE {
        return Err(last_error("FileTimeToSystemTime"));
    }
    if unsafe { SetSystemTime(&st) } == FALSE {
        return Err(last_error("SetSystemTime"));
    }
    let (s1, i1) = (SystemTime::now(), Instant::now());
    Ok(ActionReport {
        kind: "step",
        requested: offset,
        achieved: (sys_secs(s1) - sys_secs(s0)) - (i1 - i0).as_secs_f64(),
        slew_ppm: None,
        slew_seconds: None,
        time_increment_100ns: None,
    })
}
