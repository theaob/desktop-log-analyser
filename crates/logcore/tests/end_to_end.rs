use logcore::export::{export, ExportFormat};
use logcore::search::{Direction, SearchRequest};
use logcore::{open_or_build, FolderSettings, Progress};
use std::io::Write;
use std::sync::atomic::AtomicBool;

const APP_LOG: &str = "\
2026-10-08 10:00:00.000 [main] INFO  com.acme.App [req-1] - starting
2026-10-08 10:00:01.000 [http-1] ERROR com.acme.payment.Client [req-2] - call failed
java.net.SocketTimeoutException: Read timed out
\tat com.acme.payment.Client.call(Client.java:10)
2026-10-08 10:00:02.000 [http-2] WARN  com.acme.order.Repo [req-2] - slow query took=1500ms
2026-10-08 10:00:03.000 [http-1] INFO  com.acme.payment.Client [req-3] - ok took=20ms
";

const ROLLED: &str = "\
2026-10-07 09:00:00.000 [main] INFO  com.acme.App [req-0] - yesterday
2026-10-07 09:00:05.000 [main] ERROR com.acme.App [req-0] - yesterday failed
";

fn setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let logs = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(logs.path().join("app.log"), APP_LOG).unwrap();
    let mut gz = flate2::write::GzEncoder::new(
        std::fs::File::create(logs.path().join("app-2026-10-07-1.log.gz")).unwrap(),
        flate2::Compression::fast(),
    );
    gz.write_all(ROLLED.as_bytes()).unwrap();
    gz.finish().unwrap();
    (logs, data)
}

fn settings() -> Option<FolderSettings> {
    Some(FolderSettings {
        patterns: vec!["%d{yyyy-MM-dd HH:mm:ss.SSS} [%t] %-5level %logger{36} [%X{requestId}] - %msg%n".into()],
        time_zone: logcore::time::TzMode::Utc,
        ..Default::default()
    })
}

#[test]
fn index_search_page_and_export() {
    let (logs, data) = setup();
    let ws = open_or_build(logs.path(), data.path(), settings(), &Progress::default(), false).unwrap();
    assert_eq!(ws.meta.stats.events, 6);
    assert!(ws.meta.files.iter().all(|f| f.file.source == "app.log"));

    let no_cancel = AtomicBool::new(false);
    let all = logcore::search(
        &ws,
        &SearchRequest {
            limit: Some(2),
            ..Default::default()
        },
        &no_cancel,
    )
    .unwrap();
    assert_eq!(all.total, Some(6));
    assert_eq!(all.rows.len(), 2);
    assert!(all.rows[0].message.contains("ok took=20ms"), "newest first");
    let hist = all.histogram.unwrap();
    assert_eq!(hist.buckets.iter().flatten().sum::<u64>(), 6);

    // Page 2 continues where page 1 stopped.
    let page2 = logcore::search(
        &ws,
        &SearchRequest {
            limit: Some(2),
            after: all.next,
            rows_only: true,
            ..Default::default()
        },
        &no_cancel,
    )
    .unwrap();
    assert!(page2.rows[0].message.contains("call failed"));
    assert!(page2.total.is_none());

    let errors = logcore::search(
        &ws,
        &SearchRequest {
            query: r#"{level="ERROR"}"#.into(),
            ..Default::default()
        },
        &no_cancel,
    )
    .unwrap();
    assert_eq!(errors.total, Some(2));
    let exc = logcore::search(
        &ws,
        &SearchRequest {
            query: r#"{exception="java.net.SocketTimeoutException", requestId="req-2"}"#.into(),
            ..Default::default()
        },
        &no_cancel,
    )
    .unwrap();
    assert_eq!(exc.total, Some(1));
    assert_eq!(exc.rows[0].thread, "http-1");
    let slow = logcore::search(
        &ws,
        &SearchRequest {
            query: r#"{} | logfmt | took > 1s"#.into(),
            ..Default::default()
        },
        &no_cancel,
    )
    .unwrap();
    assert_eq!(slow.total, Some(1));
    assert_eq!(slow.rows[0].parsed, vec![("took".to_string(), "1500ms".to_string())]);

    // Time range: only 2026-10-08.
    let from = 1_791_417_600_000; // 2026-10-08T00:00:00Z
    let day = logcore::search(
        &ws,
        &SearchRequest {
            from: Some(from),
            ..Default::default()
        },
        &no_cancel,
    )
    .unwrap();
    assert_eq!(day.total, Some(4));

    let out = data.path().join("out.csv");
    let n = export(
        &ws,
        &SearchRequest {
            query: r#"{logger=~"com\\.acme\\.payment.*"}"#.into(),
            direction: Direction::Forward,
            ..Default::default()
        },
        &out,
        ExportFormat::Csv,
        &no_cancel,
    )
    .unwrap();
    assert_eq!(n, 2);
    let csv = std::fs::read_to_string(&out).unwrap();
    assert!(csv
        .lines()
        .nth(1)
        .unwrap()
        .starts_with("2026-10-08T10:00:01.000Z,error"));

    // Reopening with unchanged files reuses the workspace.
    let again = open_or_build(logs.path(), data.path(), None, &Progress::default(), false).unwrap();
    assert_eq!(again.meta.indexed_at_ms, ws.meta.indexed_at_ms);
}
