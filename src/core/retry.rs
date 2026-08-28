use std::time::Duration;

use chrono::{DateTime, Utc};

pub const MAX_AUTO_RETRIES: usize = 3;

pub fn parse_retry_after(value: Option<&str>) -> Duration {
    let Some(value) = value else {
        return Duration::from_secs(1);
    };
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Duration::from_secs(seconds.min(60));
    }
    if let Ok(date) = DateTime::parse_from_rfc2822(value.trim()) {
        let seconds = date
            .with_timezone(&Utc)
            .signed_duration_since(Utc::now())
            .num_seconds();
        if seconds > 0 {
            return Duration::from_secs(seconds.min(60) as u64);
        }
        return Duration::ZERO;
    }
    Duration::from_secs(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_retry_after_seconds() {
        assert_eq!(parse_retry_after(Some("2")), Duration::from_secs(2));
        assert_eq!(parse_retry_after(None), Duration::from_secs(1));
    }
}
