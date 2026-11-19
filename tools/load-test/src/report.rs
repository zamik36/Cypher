use std::fmt::Write as _;

use serde::Serialize;

use crate::stats::Percentiles;

#[derive(Debug, Serialize)]
pub(crate) struct Report {
    pub connections: usize,
    pub connected: u64,
    /// Connections on the gateway whose memory is sampled.
    pub gateway_connections: u64,
    pub connect_errors: u64,
    pub relay_errors: u64,
    pub relayed: u64,
    pub relayed_per_sec: u64,
    pub connect_us: Percentiles,
    pub relay_us: Percentiles,
    pub gateway_rss_before: Option<u64>,
    pub gateway_rss_after: Option<u64>,
    pub bytes_per_connection: Option<u64>,
    pub gateway_tasks_before: Option<u64>,
    pub gateway_tasks_after_close: Option<u64>,
}

/// Pass/fail thresholds from the command line.
#[derive(Debug, Default)]
pub(crate) struct Limits {
    pub max_errors: Option<u64>,
    pub p99_ms: Option<u64>,
    pub bytes_per_conn: Option<u64>,
    pub tasks_slack: Option<u64>,
}

impl From<&crate::args::Args> for Limits {
    fn from(args: &crate::args::Args) -> Self {
        Self {
            max_errors: args.max_errors,
            p99_ms: args.assert_p99_ms,
            bytes_per_conn: args.assert_max_bytes_per_conn,
            tasks_slack: args.assert_tasks_return,
        }
    }
}

impl Report {
    /// Memory the gateway gained per connection over the ramp.
    pub(crate) fn per_connection(
        before: Option<u64>,
        after: Option<u64>,
        connected: u64,
    ) -> Option<u64> {
        after?.saturating_sub(before?).checked_div(connected)
    }

    pub(crate) fn render(&self) -> Result<String, std::fmt::Error> {
        let mut out = String::new();
        let pct = |p: &Percentiles| {
            format!(
                "p50={} p90={} p99={} p999={} max={}",
                p.p50, p.p90, p.p99, p.p999, p.max
            )
        };
        writeln!(out, "connected:   {}/{}", self.connected, self.connections)?;
        writeln!(
            out,
            "errors:      {} connect, {} relay",
            self.connect_errors, self.relay_errors
        )?;
        writeln!(
            out,
            "relayed:     {} ({}/s)",
            self.relayed, self.relayed_per_sec
        )?;
        writeln!(out, "connect µs:  {}", pct(&self.connect_us))?;
        writeln!(out, "relay µs:    {}", pct(&self.relay_us))?;
        if let (Some(before), Some(after)) = (self.gateway_rss_before, self.gateway_rss_after) {
            let per_conn = self.bytes_per_connection.unwrap_or(0);
            writeln!(
                out,
                "gateway rss: {before} -> {after} bytes ({per_conn} per each of {} connections)",
                self.gateway_connections
            )?;
        }
        if let (Some(before), Some(after)) =
            (self.gateway_tasks_before, self.gateway_tasks_after_close)
        {
            writeln!(
                out,
                "gateway tasks: {before} before, {after} after every client left"
            )?;
        }
        Ok(out)
    }

    /// Every threshold the run broke; empty means it passed.
    pub(crate) fn violations(&self, limits: &Limits) -> Vec<String> {
        let mut broken = Vec::new();
        let errors = self.connect_errors.saturating_add(self.relay_errors);
        if let Some(max) = limits.max_errors
            && errors > max
        {
            broken.push(format!("{errors} errors > {max}"));
        }
        if let Some(ms) = limits.p99_ms
            && self.relay_us.p99 > ms.saturating_mul(1000)
        {
            broken.push(format!("p99 {} µs > {ms} ms", self.relay_us.p99));
        }
        match (limits.bytes_per_conn, self.bytes_per_connection) {
            (Some(max), Some(actual)) if actual > max => {
                broken.push(format!("{actual} bytes per connection > {max}"));
            }
            (Some(_), None) => broken.push("gateway memory was not measured".to_owned()),
            _ => {}
        }
        if let Some(slack) = limits.tasks_slack {
            match (self.gateway_tasks_before, self.gateway_tasks_after_close) {
                (Some(before), Some(after)) if after > before.saturating_add(slack) => broken.push(
                    format!("gateway kept {after} tasks after every client left (was {before})"),
                ),
                (Some(_), Some(_)) => {}
                _ => broken.push("gateway tasks were not measured".to_owned()),
            }
        }
        broken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            connections: 10,
            connected: 10,
            gateway_connections: 10,
            connect_errors: 0,
            relay_errors: 1,
            relayed: 500,
            relayed_per_sec: 50,
            connect_us: Percentiles::default(),
            relay_us: Percentiles {
                p99: 40_000,
                ..Percentiles::default()
            },
            gateway_rss_before: Some(1_000),
            gateway_rss_after: Some(21_000),
            bytes_per_connection: Some(2_000),
            gateway_tasks_before: Some(40),
            gateway_tasks_after_close: Some(42),
        }
    }

    #[test]
    fn thresholds_report_each_violation() {
        let r = report();
        assert!(r.violations(&Limits::default()).is_empty());
        let strict = Limits {
            max_errors: Some(0),
            p99_ms: Some(10),
            bytes_per_conn: Some(1_000),
            tasks_slack: Some(1),
        };
        assert_eq!(r.violations(&strict).len(), 4);
        let loose = Limits {
            max_errors: Some(1),
            p99_ms: Some(50),
            bytes_per_conn: Some(4_096),
            tasks_slack: Some(2),
        };
        assert!(r.violations(&loose).is_empty());
        let unmeasured = Report {
            bytes_per_connection: None,
            ..report()
        };
        assert_eq!(
            unmeasured.violations(&loose),
            ["gateway memory was not measured"]
        );
        let tasks_unknown = Report {
            gateway_tasks_after_close: None,
            ..report()
        };
        assert_eq!(
            tasks_unknown.violations(&loose),
            ["gateway tasks were not measured"]
        );
    }

    #[test]
    fn memory_per_connection() {
        assert_eq!(
            Report::per_connection(Some(1_000), Some(21_000), 10),
            Some(2_000)
        );
        assert_eq!(Report::per_connection(Some(1_000), Some(900), 10), Some(0));
        assert_eq!(Report::per_connection(None, Some(900), 10), None);
        assert_eq!(Report::per_connection(Some(1), Some(9), 0), None);
    }

    #[test]
    fn text_report_mentions_memory_only_when_measured() {
        let text = report().render().unwrap();
        assert!(text.contains("errors:      0 connect, 1 relay"));
        assert!(text.contains("2000 per each of 10 connections"));
        assert!(text.contains("gateway tasks: 40 before, 42 after"));
        let unmeasured = Report {
            gateway_rss_before: None,
            ..report()
        };
        assert!(!unmeasured.render().unwrap().contains("gateway rss"));
    }
}
