use std::env;

use log::{warn, LevelFilter};

use crate::Opt;

fn set_default_logger(log_level: LevelFilter) {
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", log_level.to_string());
    }
    env_logger::init();
}

pub fn initialize_logging(opt: &Opt) {
    let log_level = if opt.compat_debug {
        LevelFilter::Debug
    } else {
        opt.log_level
    };

    if opt.syslog {
        if let Err(e) = syslog::init(syslog::Facility::LOG_USER, log_level, None) {
            set_default_logger(log_level);
            warn!("can't enable syslog: {e}");
        }
    } else {
        set_default_logger(log_level);
    }
}
