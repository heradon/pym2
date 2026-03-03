use crate::error::{PyopsError, Result};

#[derive(Debug, Clone, Copy)]
pub enum RestartSchedule {
    Daily {
        hour: u8,
        minute: u8,
    },
    Weekly {
        weekday: Weekday,
        hour: u8,
        minute: u8,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "mon" | "monday" => Some(Self::Mon),
            "tue" | "tuesday" => Some(Self::Tue),
            "wed" | "wednesday" => Some(Self::Wed),
            "thu" | "thursday" => Some(Self::Thu),
            "fri" | "friday" => Some(Self::Fri),
            "sat" | "saturday" => Some(Self::Sat),
            "sun" | "sunday" => Some(Self::Sun),
            _ => None,
        }
    }

    fn to_tm_wday(self) -> i32 {
        match self {
            Self::Sun => 0,
            Self::Mon => 1,
            Self::Tue => 2,
            Self::Wed => 3,
            Self::Thu => 4,
            Self::Fri => 5,
            Self::Sat => 6,
        }
    }
}

pub fn parse_restart_schedule(input: &str) -> Result<RestartSchedule> {
    let raw = input.trim();

    if let Some(time_part) = raw.strip_prefix("daily@") {
        let (hour, minute) = parse_hhmm(time_part)?;
        return Ok(RestartSchedule::Daily { hour, minute });
    }

    if let Some(rest) = raw.strip_prefix("weekly@") {
        let mut parts = rest.split_whitespace();
        let day = parts.next().ok_or_else(|| {
            PyopsError::Config(format!("invalid weekly schedule '{}': missing day", input))
        })?;
        let time = parts.next().ok_or_else(|| {
            PyopsError::Config(format!("invalid weekly schedule '{}': missing time", input))
        })?;
        if parts.next().is_some() {
            return Err(PyopsError::Config(format!(
                "invalid weekly schedule '{}': expected format weekly@sun 03:00",
                input
            )));
        }

        let weekday = Weekday::from_str(day).ok_or_else(|| {
            PyopsError::Config(format!(
                "invalid weekday '{}' in schedule '{}'; use mon..sun",
                day, input
            ))
        })?;
        let (hour, minute) = parse_hhmm(time)?;
        return Ok(RestartSchedule::Weekly {
            weekday,
            hour,
            minute,
        });
    }

    Err(PyopsError::Config(format!(
        "unsupported restart_schedule '{}'; use daily@HH:MM or weekly@sun HH:MM",
        input
    )))
}

fn parse_hhmm(input: &str) -> Result<(u8, u8)> {
    let mut parts = input.split(':');
    let hour_str = parts
        .next()
        .ok_or_else(|| PyopsError::Config(format!("invalid time '{}': expected HH:MM", input)))?;
    let min_str = parts
        .next()
        .ok_or_else(|| PyopsError::Config(format!("invalid time '{}': expected HH:MM", input)))?;
    if parts.next().is_some() {
        return Err(PyopsError::Config(format!(
            "invalid time '{}': expected HH:MM",
            input
        )));
    }

    let hour: u8 = hour_str
        .parse()
        .map_err(|_| PyopsError::Config(format!("invalid hour '{}'", hour_str)))?;
    let minute: u8 = min_str
        .parse()
        .map_err(|_| PyopsError::Config(format!("invalid minute '{}'", min_str)))?;

    if hour > 23 || minute > 59 {
        return Err(PyopsError::Config(format!(
            "invalid time '{}': hour must be 0..23 and minute 0..59",
            input
        )));
    }

    Ok((hour, minute))
}

pub fn next_occurrence(schedule: RestartSchedule, now_epoch: u64) -> Option<u64> {
    match schedule {
        RestartSchedule::Daily { hour, minute } => next_daily(hour, minute, now_epoch),
        RestartSchedule::Weekly {
            weekday,
            hour,
            minute,
        } => next_weekly(weekday, hour, minute, now_epoch),
    }
}

fn next_daily(hour: u8, minute: u8, now_epoch: u64) -> Option<u64> {
    let tm = local_tm(now_epoch as i64)?;

    let mut candidate = tm;
    candidate.tm_hour = i32::from(hour);
    candidate.tm_min = i32::from(minute);
    candidate.tm_sec = 0;

    let mut ts = mktime_local(&mut candidate)?;
    if ts <= now_epoch as i64 {
        candidate.tm_mday += 1;
        ts = mktime_local(&mut candidate)?;
    }

    Some(ts as u64)
}

fn next_weekly(weekday: Weekday, hour: u8, minute: u8, now_epoch: u64) -> Option<u64> {
    let tm = local_tm(now_epoch as i64)?;
    let current = tm.tm_wday;
    let target = weekday.to_tm_wday();

    let mut delta_days = target - current;
    if delta_days < 0 {
        delta_days += 7;
    }

    let mut candidate = tm;
    candidate.tm_mday += delta_days;
    candidate.tm_hour = i32::from(hour);
    candidate.tm_min = i32::from(minute);
    candidate.tm_sec = 0;

    let mut ts = mktime_local(&mut candidate)?;
    if ts <= now_epoch as i64 {
        candidate.tm_mday += 7;
        ts = mktime_local(&mut candidate)?;
    }

    Some(ts as u64)
}

fn local_tm(epoch: i64) -> Option<libc::tm> {
    let mut ts: libc::time_t = epoch as libc::time_t;
    let mut out = std::mem::MaybeUninit::<libc::tm>::uninit();
    let ptr = unsafe { libc::localtime_r(&mut ts as *mut libc::time_t, out.as_mut_ptr()) };
    if ptr.is_null() {
        return None;
    }
    Some(unsafe { out.assume_init() })
}

fn mktime_local(tm: &mut libc::tm) -> Option<i64> {
    let ts = unsafe { libc::mktime(tm as *mut libc::tm) };
    if ts < 0 {
        return None;
    }
    Some(ts as i64)
}
