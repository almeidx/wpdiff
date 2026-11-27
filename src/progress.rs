use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub fn bar(len: u64, template: &str) -> ProgressBar {
    show(ProgressBar::new(len), style(template).progress_chars("█▓░"))
}

pub fn spinner(template: &str) -> ProgressBar {
    show(ProgressBar::new_spinner(), style(template))
}

fn style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template).expect("progress templates are valid literals")
}

/// Bars started from rayon workers would redraw over each other, so only
/// progress created outside the pool is drawn.
fn show(pb: ProgressBar, style: ProgressStyle) -> ProgressBar {
    if rayon::current_thread_index().is_some() {
        return ProgressBar::hidden();
    }
    pb.set_style(style);
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

#[cfg(test)]
mod tests {
    use super::*;

    // `is_hidden()` is also true whenever stderr is not a terminal, so the
    // test tells a real bar from a hidden one by its length instead.
    #[test]
    fn only_progress_from_parallel_workers_is_hidden() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let from_worker = pool.install(|| bar(100, "{pos}/{len}"));

        assert_eq!(from_worker.length(), None);
        assert_eq!(bar(100, "{pos}/{len}").length(), Some(100));
    }
}
