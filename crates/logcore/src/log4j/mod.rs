//! log4j / Logback support: PatternLayout compilation, common default patterns and the
//! JSON layouts (`JsonTemplateLayout`, `JSONLayout` compact, Logstash encoder).

pub mod pattern;

pub use pattern::{Capture, CompiledPattern, PatternError};

/// Patterns tried, in order, when the user has not supplied one. They cover log4j 2 and
/// 1.x defaults, common Logback configs and Spring Boot's default console/file pattern.
pub const COMMON_PATTERNS: &[&str] = &[
    "%d{yyyy-MM-dd HH:mm:ss.SSS} [%t] %-5level %logger{36} - %msg%n",
    "%d{ISO8601} [%t] %-5p %c - %m%n",
    "%d [%t] %-5p %c - %m%n",
    "%d{HH:mm:ss.SSS} [%t] %-5level %logger{36} - %msg%n",
    "%d %-5p [%t] %c - %m%n",
    "%d %-5p [%c] (%t) %m%n",
    "%d{ISO8601} %-5p [%t] %c{1}:%L - %m%n",
    "%d{yyyy-MM-dd HH:mm:ss} %-5p %c{1}:%L - %m%n",
    "%d{yyyy-MM-dd HH:mm:ss.SSS} %-5level [%thread] %logger{36} - %msg%n",
    "%d{yyyy-MM-dd HH:mm:ss.SSS} %-5level %logger{36} - %msg%n",
    "%d{yyyy-MM-dd'T'HH:mm:ss.SSSXXX} %5p %pid --- [%t] %-40.40logger{39} : %m%n",
    "%clr(%d{${LOG_DATEFORMAT_PATTERN:-yyyy-MM-dd'T'HH:mm:ss.SSSXXX}}){faint} %clr(${LOG_LEVEL_PATTERN:-%5p}) %clr(${PID:- }){magenta} %clr(---){faint} %clr([%15.15t]){faint} %clr(%-40.40logger{39}){cyan} %clr(:){faint} %m%n",
    "%-5p %d [%t] %c: %m%n",
    "%-5p [%t] %d{ISO8601} %c - %m%n",
    "%r [%t] %-5p %c %x - %m%n",
];

/// Field names used for well-known keys in JSON layouts.
pub(crate) mod json_keys {
    pub const TIMESTAMP: &[&str] = &[
        "@timestamp",
        "timestamp",
        "time",
        "ts",
        "date",
        "datetime",
        "timeMillis",
    ];
    pub const LEVEL: &[&str] = &["level", "log.level", "severity", "loglevel", "lvl"];
    pub const LOGGER: &[&str] = &["loggerName", "logger_name", "logger", "log.logger", "category"];
    pub const THREAD: &[&str] = &["thread", "thread_name", "threadName", "process.thread.name"];
    pub const MESSAGE: &[&str] = &["message", "msg", "log"];
    pub const MDC: &[&str] = &["contextMap", "mdc", "context", "labels"];
}
