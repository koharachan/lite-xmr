use tracing_subscriber::fmt::{format::Writer, time::FormatTime};

struct LocalLogTime;

impl FormatTime for LocalLogTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%y/%m/%d %H:%M:%S"))
    }
}

pub fn init(level: &str) {
    tracing_subscriber::fmt()
        .compact()
        .with_timer(LocalLogTime)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| level.parse().unwrap()),
        )
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .init();
}
