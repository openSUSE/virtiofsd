use std::ffi::CString;

use tracing::level_filters::LevelFilter;
use tracing::{field, Event, Level, Subscriber};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::{layer, EnvFilter, Layer};

use crate::Opt;

struct SyslogLayer;
impl SyslogLayer {
    fn new() -> Self {
        // SAFETY: Safe because all arguments are valid.
        // If the first argument is `NULL`, the program name is used.
        unsafe {
            libc::openlog(
                std::ptr::null(),
                libc::LOG_NDELAY | libc::LOG_PID,
                libc::LOG_USER,
            );
        }
        SyslogLayer
    }
}

impl<S: Subscriber> Layer<S> for SyslogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: layer::Context<'_, S>) {
        let priority = match *event.metadata().level() {
            Level::ERROR => libc::LOG_ERR,
            Level::WARN => libc::LOG_WARNING,
            Level::INFO => libc::LOG_INFO,
            Level::DEBUG | Level::TRACE => libc::LOG_DEBUG,
        };

        let mut visitor = FieldCollector::new();
        event.record(&mut visitor);

        let bytes = visitor
            .into_string()
            .into_bytes()
            .into_iter()
            .map(|c| if c == 0 { b'?' } else { c })
            .collect();
        // SAFETY: `bytes` doesn't contains interior NUL bytes
        let msg = unsafe { CString::from_vec_unchecked(bytes) };

        // SAFETY: Safe because `msg` is valid by construction.
        unsafe {
            libc::syslog(
                priority,
                b"%s\0".as_ptr().cast::<libc::c_char>(),
                msg.as_ptr(),
            );
        }
    }
}

impl Drop for SyslogLayer {
    fn drop(&mut self) {
        // SAFETY: Safe because is called after `openlog()`.
        unsafe { libc::closelog() };
    }
}

struct FieldCollector {
    fields: Vec<String>,
}

impl FieldCollector {
    fn new() -> Self {
        Self {
            // Start with small capacity to avoid reallocations
            fields: Vec::with_capacity(5),
        }
    }

    fn into_string(self) -> String {
        self.fields.join(", ")
    }
}

impl field::Visit for FieldCollector {
    fn record_debug(&mut self, field: &field::Field, value: &dyn std::fmt::Debug) {
        // If a format string is provided, tracing implicitly creates a field named "message".
        if field.name() == "message" {
            self.fields.insert(0, format!("{value:?}"));
        } else {
            self.fields.push(format!("{}={:?}", field.name(), value));
        }
    }
}

pub(super) fn initialize_logging(opt: &Opt) {
    use tracing_subscriber::prelude::*;

    let log_level = if opt.compat_debug {
        LevelFilter::DEBUG
    } else {
        opt.log_level
    };

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level.to_string()));

    if opt.syslog {
        tracing_subscriber::registry()
            .with(SyslogLayer::new())
            .with(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_span_events(FmtSpan::CLOSE) // Emit an event when an instrumented function returns.
            .with_thread_ids(true)
            .init();
    }
}
