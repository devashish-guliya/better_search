//! Number formatting for log lines and console output.

use std::time::Duration;

pub fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn bytes(bytes: usize) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    format!("{:.1} MB", bytes as f64 / MB)
}

pub fn duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        format!("{ms:.1} ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_counts() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1234567), "1,234,567");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(Duration::from_micros(1500)), "1.5 ms");
        assert_eq!(duration(Duration::from_millis(2500)), "2.50 s");
    }
}
