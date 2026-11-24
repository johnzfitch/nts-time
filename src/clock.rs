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
const MAX_SLEW_SECONDS: f64 = 300.0;

pub fn validate_settings(step_threshold: f64, max_slew_ppm: f64) -> Result<(), String> {
    if !step_threshold.is_finite() || step_threshold < 0.0 {
        return Err("--step-threshold must be finite and nonnegative".into());
    }
    if !max_slew_ppm.is_finite() || max_slew_ppm <= 0.0 || max_slew_ppm > 500.0 {
        return Err("--max-slew-ppm must be finite, greater than zero, and at most 500".into());
    }
    Ok(())
}

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
        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        if LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) == FALSE {
            CloseHandle(token);
            return Err(last_error("LookupPrivilegeValueW"));
        }
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [windows_sys::Win32::Security::LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let ok = AdjustTokenPrivileges(
            token,
            FALSE,
            &tp,
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
fn adjustment_for(inc: u64, delta: u64, faster: bool) -> Option<u64> {
    if delta >= inc {
        return None;
    }
    if faster {
        inc.checked_sub(delta)
    } else {
        inc.checked_add(delta)
    }
}

fn slew_plan(inc: u64, offset: f64, ppm: f64) -> Option<(u64, f64, Duration)> {
    if !offset.is_finite() || validate_settings(0.0, ppm).is_err() {
        return None;
    }
    let delta = (inc as f64 * ppm * 1e-6).round() as u64;
    if delta == 0 {
        return None;
    }
    let adjustment = adjustment_for(inc, delta, offset > 0.0)?;
    let rate = delta as f64 / inc as f64;
    let seconds = offset.abs() / rate;
    if !seconds.is_finite() || seconds > MAX_SLEW_SECONDS {
        return None;
    }
    let duration = Duration::try_from_secs_f64(seconds).ok()?;
    Some((adjustment, rate, duration))
}

pub fn correct(
    offset: f64,
    step_threshold: f64,
    max_slew_ppm: f64,
) -> Result<ActionReport, String> {
    validate_settings(step_threshold, max_slew_ppm)?;
    if !offset.is_finite() {
        return Err("clock offset must be finite".into());
    }
    if offset.abs() >= step_threshold {
        return step(offset);
    }
    let (mut adj, mut inc, mut disabled) = (0u64, 0u64, 0);
    if unsafe { GetSystemTimeAdjustmentPrecise(&mut adj, &mut inc, &mut disabled) } == FALSE {
        return Err(last_error("GetSystemTimeAdjustmentPrecise"));
    }
    let Some((new_adj, rate, duration)) = slew_plan(inc, offset, max_slew_ppm) else {
        return step(offset); // unrepresentable rate or over five minutes: use a bounded step
    };
    let seconds = duration.as_secs_f64();

    let _guard = ResetOnDrop;
    let (s0, i0) = (SystemTime::now(), Instant::now());
    if unsafe { SetSystemTimeAdjustmentPrecise(new_adj, FALSE) } == FALSE {
        return Err(last_error("SetSystemTimeAdjustmentPrecise"));
    }
    std::thread::sleep(duration);
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
    validate_settings(0.0, ppm)?;
    if !secs.is_finite() || secs <= 0.0 || secs > MAX_SLEW_SECONDS {
        return Err("diagnostic duration must be between 0 and 300 seconds".into());
    }
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
        let new_adj = if sign == 0 {
            inc
        } else {
            adjustment_for(inc, delta, sign > 0).ok_or("unrepresentable clock adjustment")?
        };
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
    let target = step_target(now_100ns, offset)?;
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

fn step_target(now_100ns: i128, offset: f64) -> Result<u64, String> {
    if !offset.is_finite() {
        return Err("clock offset must be finite".into());
    }
    let target = now_100ns
        .checked_add(UNIX_TO_FILETIME_100NS)
        .and_then(|t| t.checked_add((offset * 1e7).round() as i128))
        .and_then(|t| t.checked_add(5_000))
        .ok_or("clock target overflow")?;
    if !(0..=i64::MAX as i128).contains(&target) {
        return Err("clock target is outside the Windows FILETIME range".into());
    }
    let target = target - target.rem_euclid(10_000);
    Ok(target as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slew_plan_preserves_direction_and_default_duration() {
        let (fast, rate, duration) = slew_plan(10_000_000, 0.1, 500.0).unwrap();
        assert_eq!(fast, 9_995_000);
        assert_eq!(rate, 0.0005);
        assert_eq!(duration, Duration::from_secs(200));
        assert_eq!(slew_plan(10_000_000, -0.1, 500.0).unwrap().0, 10_005_000);
    }

    #[test]
    fn unrepresentable_and_overlong_slews_fall_back_to_steps() {
        for (inc, offset, ppm) in [
            (0, 0.1, 500.0),
            (1, 0.1, 500.0),
            (10_000_000, 0.1, 0.001),
            (10_000_000, 1.0, 500.0),
            (10_000_000, f64::NAN, 500.0),
            (10_000_000, 0.1, f64::INFINITY),
            (10_000_000, 0.1, 1_000_000.0),
        ] {
            assert!(slew_plan(inc, offset, ppm).is_none());
        }
        assert_eq!(adjustment_for(10, 10, true), None);
        assert_eq!(adjustment_for(10, 11, true), None);
        assert_eq!(adjustment_for(u64::MAX, 1, false), None);
    }

    #[test]
    fn step_targets_are_checked_before_narrowing_to_filetime() {
        assert_eq!(step_target(0, 0.0).unwrap(), UNIX_TO_FILETIME_100NS as u64);
        assert_eq!(
            step_target(0, 0.0016).unwrap(),
            UNIX_TO_FILETIME_100NS as u64 + 20_000
        );
        for offset in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            -f64::MAX,
            -20_000_000_000.0,
        ] {
            assert!(step_target(0, offset).is_err());
        }
        assert!(step_target(i128::MAX, 1.0).is_err());
        assert!(step_target(i128::MIN, 0.0).is_err());
    }
}
