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
