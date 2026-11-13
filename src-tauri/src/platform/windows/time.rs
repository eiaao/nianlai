// 本地日期（备份默认文件名、托盘“今天记下”的起点）。
use windows::Win32::{
    Foundation::{FILETIME, SYSTEMTIME},
    System::{
        SystemInformation::GetLocalTime,
        Time::{SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime},
    },
};

// 1601-01-01（FILETIME 起点）到 1970-01-01 的 100ns 间隔数。
const WINDOWS_EPOCH_OFFSET_100NS: u64 = 116_444_736_000_000_000;

/// 本地时区的（年，月，日）。
pub fn local_date() -> (u16, u8, u8) {
    let now = unsafe { GetLocalTime() };
    (now.wYear, now.wMonth as u8, now.wDay as u8)
}

fn file_time_to_epoch_ms(ticks: u64) -> Option<u64> {
    Some(ticks.checked_sub(WINDOWS_EPOCH_OFFSET_100NS)? / 10_000)
}

/// 用同一份本地时间确定日期，再由 Windows 按当天时区/DST 规则把本地零点换成 UTC epoch。
/// 不再分别读取本地时钟和 UTC 时钟，避免跨午夜时拼出昨天的起点。
fn local_day_start_from(now: SYSTEMTIME) -> Option<u64> {
    let midnight = SYSTEMTIME {
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
        ..now
    };
    let mut utc = SYSTEMTIME::default();
    unsafe { TzSpecificLocalTimeToSystemTime(None, &midnight, &mut utc) }.ok()?;
    let mut file_time = FILETIME::default();
    unsafe { SystemTimeToFileTime(&utc, &mut file_time) }.ok()?;
    let ticks = (u64::from(file_time.dwHighDateTime) << 32) | u64::from(file_time.dwLowDateTime);
    file_time_to_epoch_ms(ticks)
}

/// 本机自然日 00:00 对应的 epoch 毫秒，与 copy_events.time_ms 对齐。
pub fn local_day_start_ms() -> u64 {
    let now = unsafe { GetLocalTime() };
    local_day_start_from(now).unwrap_or_else(|| {
        // Win32 转换失败时保留旧退路；饱和减法避免 epoch 之前的下溢。
        let elapsed = u64::from(now.wHour) * 3_600_000
            + u64::from(now.wMinute) * 60_000
            + u64::from(now.wSecond) * 1000
            + u64::from(now.wMilliseconds);
        crate::log::now_ms().saturating_sub(elapsed)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{
        Foundation::{FILETIME, SYSTEMTIME},
        System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
    };

    fn local(hour: u16, minute: u16, second: u16, milliseconds: u16) -> SYSTEMTIME {
        SYSTEMTIME {
            wYear: 2026,
            wMonth: 11,
            wDay: 12,
            wHour: hour,
            wMinute: minute,
            wSecond: second,
            wMilliseconds: milliseconds,
            ..Default::default()
        }
    }

    #[test]
    fn health_local_day_start_uses_one_local_date_and_converts_midnight_to_epoch() {
        for time in [(0, 0, 0, 0), (23, 59, 59, 999)] {
            let start = local_day_start_from(local(time.0, time.1, time.2, time.3)).unwrap();
            let ticks = WINDOWS_EPOCH_OFFSET_100NS + start * 10_000;
            let file_time = FILETIME {
                dwLowDateTime: ticks as u32,
                dwHighDateTime: (ticks >> 32) as u32,
            };
            let mut utc = SYSTEMTIME::default();
            unsafe { FileTimeToSystemTime(&file_time, &mut utc) }.unwrap();
            let mut roundtrip = SYSTEMTIME::default();
            unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut roundtrip) }.unwrap();
            assert_eq!(
                (roundtrip.wYear, roundtrip.wMonth, roundtrip.wDay),
                (2026, 11, 12)
            );
            assert_eq!(
                (
                    roundtrip.wHour,
                    roundtrip.wMinute,
                    roundtrip.wSecond,
                    roundtrip.wMilliseconds
                ),
                (0, 0, 0, 0)
            );
        }
    }

    #[test]
    fn health_filetime_epoch_conversion_checks_underflow_and_overflow() {
        assert_eq!(file_time_to_epoch_ms(WINDOWS_EPOCH_OFFSET_100NS - 1), None);
        assert_eq!(file_time_to_epoch_ms(WINDOWS_EPOCH_OFFSET_100NS), Some(0));
        assert_eq!(
            file_time_to_epoch_ms(WINDOWS_EPOCH_OFFSET_100NS + 10_000),
            Some(1)
        );
        assert_eq!(
            file_time_to_epoch_ms(u64::MAX),
            Some((u64::MAX - WINDOWS_EPOCH_OFFSET_100NS) / 10_000)
        );
    }
}
