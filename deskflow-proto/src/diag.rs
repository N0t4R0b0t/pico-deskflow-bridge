//! Diagnostics for the settings page: input latency, stack use and the
//! rest of what the firmware measures.

use core::fmt;

/// Average and worst latency over a window of reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Latency {
    pub avg_us: u32,
    pub max_us: u32,
}

/// Collects report latencies (Deskflow message in → sent over Bluetooth)
/// and publishes their average and maximum every [`LatencyWindow::LEN`].
pub struct LatencyWindow {
    sum: u64,
    max: u32,
    count: u32,
    last: Option<Latency>,
}

impl LatencyWindow {
    pub const LEN: u32 = 64;

    pub const fn new() -> Self {
        LatencyWindow { sum: 0, max: 0, count: 0, last: None }
    }

    pub fn add(&mut self, us: u32) {
        self.sum += us as u64;
        self.max = self.max.max(us);
        self.count += 1;
        if self.count == Self::LEN {
            self.last = Some(Latency { avg_us: (self.sum / Self::LEN as u64) as u32, max_us: self.max });
            *self = LatencyWindow { last: self.last, ..LatencyWindow::new() };
        }
    }

    /// The last full window.
    pub fn last(&self) -> Option<Latency> {
        self.last
    }
}

impl Default for LatencyWindow {
    fn default() -> Self {
        Self::new()
    }
}

/// Bytes at the low end of a descending stack still holding `fill`: the
/// part never used since it was filled.
pub fn stack_unused(stack: &[u32], fill: u32) -> usize {
    4 * stack.iter().take_while(|&&w| w == fill).count()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Room {
    pub free: usize,
    pub size: usize,
}

pub struct Diagnostics {
    pub ble_rssi: Option<i8>,
    /// Bluetooth connection interval, while connected.
    pub ble_interval_us: Option<u32>,
    /// Connections since boot.
    pub ble_connections: u32,
    /// Deskflow sessions since boot (greeted by the server).
    pub deskflow_sessions: u32,
    pub latency: Option<Latency>,
    /// Socket to parsed message, through TLS on core 1. `None` without TLS.
    pub tls_path: Option<Latency>,
    /// Bytes queued in that path as a move is handled, in place of
    /// microseconds. `None` without TLS.
    pub backlog: Option<Latency>,
    /// Packets in the Bluetooth controller after a pointer report, in place
    /// of microseconds. Each waits for those ahead of it.
    pub in_chip: Option<Latency>,
    /// Moves sent without the last one being reported sent in time.
    pub flow_timeouts: u32,
    pub heap: Room,
    /// Never-used stack, core 0 and core 1.
    pub stacks: [Room; 2],
}

impl Diagnostics {
    pub fn write_json(&self, w: &mut impl fmt::Write) -> fmt::Result {
        fn opt(w: &mut impl fmt::Write, v: Option<impl fmt::Display>) -> fmt::Result {
            match v {
                Some(v) => write!(w, "{}", v),
                None => w.write_str("null"),
            }
        }
        w.write_str(r#"{"ble_rssi":"#)?;
        opt(w, self.ble_rssi)?;
        w.write_str(r#","ble_interval_us":"#)?;
        opt(w, self.ble_interval_us)?;
        write!(w, r#","ble_connections":{},"deskflow_sessions":{},"latency_avg_us":"#, self.ble_connections, self.deskflow_sessions)?;
        opt(w, self.latency.map(|l| l.avg_us))?;
        w.write_str(r#","latency_max_us":"#)?;
        opt(w, self.latency.map(|l| l.max_us))?;
        w.write_str(r#","tls_path_avg_us":"#)?;
        opt(w, self.tls_path.map(|l| l.avg_us))?;
        w.write_str(r#","tls_path_max_us":"#)?;
        opt(w, self.tls_path.map(|l| l.max_us))?;
        w.write_str(r#","backlog_avg":"#)?;
        opt(w, self.backlog.map(|l| l.avg_us))?;
        w.write_str(r#","backlog_max":"#)?;
        opt(w, self.backlog.map(|l| l.max_us))?;
        w.write_str(r#","in_chip_avg":"#)?;
        opt(w, self.in_chip.map(|l| l.avg_us))?;
        w.write_str(r#","in_chip_max":"#)?;
        opt(w, self.in_chip.map(|l| l.max_us))?;
        write!(w, r#","flow_timeouts":{}"#, self.flow_timeouts)?;
        write!(
            w,
            r#","heap_free":{},"heap_size":{},"stack0_free":{},"stack0_size":{},"stack1_free":{},"stack1_size":{}}}"#,
            self.heap.free, self.heap.size, self.stacks[0].free, self.stacks[0].size, self.stacks[1].free, self.stacks[1].size
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_window() {
        let mut w = LatencyWindow::new();
        for i in 0..LatencyWindow::LEN - 1 {
            w.add(1000 + i);
        }
        assert_eq!(w.last(), None);
        w.add(9000);
        // 1000..=1062 and 9000.
        let sum: u64 = (1000..1063u64).sum::<u64>() + 9000;
        assert_eq!(w.last(), Some(Latency { avg_us: (sum / 64) as u32, max_us: 9000 }));
        // The next window starts fresh but keeps showing the last one.
        w.add(5);
        assert_eq!(w.last().unwrap().max_us, 9000);
        for _ in 1..LatencyWindow::LEN {
            w.add(5);
        }
        assert_eq!(w.last(), Some(Latency { avg_us: 5, max_us: 5 }));
    }

    #[test]
    fn stack_high_water() {
        let fill = 0xCCCC_CCCC;
        let mut stack = [fill; 16];
        assert_eq!(stack_unused(&stack, fill), 64);
        stack[10..].fill(0);
        assert_eq!(stack_unused(&stack, fill), 40);
        stack[0] = 1;
        assert_eq!(stack_unused(&stack, fill), 0);
    }

    #[test]
    fn json() {
        let mut d = Diagnostics {
            ble_rssi: Some(-58),
            ble_interval_us: Some(7500),
            ble_connections: 2,
            deskflow_sessions: 3,
            latency: Some(Latency { avg_us: 4200, max_us: 15000 }),
            tls_path: Some(Latency { avg_us: 300, max_us: 900 }),
            backlog: Some(Latency { avg_us: 40, max_us: 120 }),
            in_chip: Some(Latency { avg_us: 1, max_us: 4 }),
            flow_timeouts: 7,
            heap: Room { free: 30000, size: 32768 },
            stacks: [Room { free: 200000, size: 274000 }, Room { free: 12000, size: 32768 }],
        };
        let mut out = std::string::String::new();
        d.write_json(&mut out).unwrap();
        assert_eq!(
            out,
            r#"{"ble_rssi":-58,"ble_interval_us":7500,"ble_connections":2,"deskflow_sessions":3,"latency_avg_us":4200,"latency_max_us":15000,"tls_path_avg_us":300,"tls_path_max_us":900,"backlog_avg":40,"backlog_max":120,"in_chip_avg":1,"in_chip_max":4,"flow_timeouts":7,"heap_free":30000,"heap_size":32768,"stack0_free":200000,"stack0_size":274000,"stack1_free":12000,"stack1_size":32768}"#
        );
        d.ble_rssi = None;
        d.ble_interval_us = None;
        d.latency = None;
        d.tls_path = None;
        d.backlog = None;
        d.in_chip = None;
        out.clear();
        d.write_json(&mut out).unwrap();
        assert!(out.starts_with(r#"{"ble_rssi":null,"ble_interval_us":null,"ble_connections":2,"deskflow_sessions":3,"latency_avg_us":null,"latency_max_us":null,"tls_path_avg_us":null,"tls_path_max_us":null,"backlog_avg":null,"backlog_max":null,"in_chip_avg":null,"in_chip_max":null,"flow_timeouts":7,"#));
    }
}
