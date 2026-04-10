use indicatif::{ProgressBar, ProgressStyle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static SUPPRESS: AtomicBool = AtomicBool::new(false);

pub fn suppress(yes: bool) {
    SUPPRESS.store(yes, Ordering::Relaxed);
}

fn is_suppressed() -> bool {
    SUPPRESS.load(Ordering::Relaxed)
}

pub fn bar(len: u64, template: &str) -> ProgressBar {
    if is_suppressed() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new(len);
    pb.set_style(
        ProgressStyle::with_template(template)
            .unwrap()
            .progress_chars("█▓░"),
    );
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

pub fn spinner(template: &str) -> ProgressBar {
    if is_suppressed() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::with_template(template).unwrap());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppress_returns_hidden_bar() {
        suppress(true);
        let pb = bar(100, "{pos}/{len}");
        assert!(pb.is_hidden());
        suppress(false);
    }

    #[test]
    fn suppress_returns_hidden_spinner() {
        suppress(true);
        let pb = spinner("{spinner} working...");
        assert!(pb.is_hidden());
        suppress(false);
    }

    #[test]
    fn suppress_toggle() {
        suppress(true);
        assert!(is_suppressed());
        suppress(false);
        assert!(!is_suppressed());
    }
}
