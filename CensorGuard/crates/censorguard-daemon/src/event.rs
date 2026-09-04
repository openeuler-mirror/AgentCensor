use crate::runtime::Runtime;
use censorguard_common::abi::RawEvent;
use censorguard_common::protocol::Event;
use censorguard_kernel::NativeKernel;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Subscriber {
    sender: SyncSender<Event>,
    dropped: u64,
}

pub struct EventHub {
    subscribers: Mutex<Vec<Subscriber>>,
    subscriber_dropped: AtomicU64,
    kernel_dropped: AtomicU64,
    reader_dropped: AtomicU64,
    sequence: AtomicU64,
    boot_id: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EventStats {
    pub kernel_dropped: u64,
    pub reader_dropped: u64,
    pub subscriber_dropped: u64,
    pub subscribers: usize,
}

impl EventHub {
    pub fn new(boot_id: String) -> Self {
        Self {
            subscribers: Mutex::new(Vec::new()),
            subscriber_dropped: AtomicU64::new(0),
            kernel_dropped: AtomicU64::new(0),
            reader_dropped: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            boot_id,
        }
    }

    pub fn subscribe(&self) -> Receiver<Event> {
        let (sender, receiver) = sync_channel(256);
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(Subscriber { sender, dropped: 0 });
        }
        receiver
    }

    fn broadcast(&self, mut event: Event) {
        // Sequence starts at 1; 0 is the "unset" sentinel that consumers filter on.
        event.sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        event.daemon_boot_id = self.boot_id.clone();
        let Ok(mut subscribers) = self.subscribers.lock() else {
            return;
        };
        subscribers.retain_mut(|subscriber| {
            let mut frame = event.clone();
            frame.dropped_before = subscriber.dropped;
            match subscriber.sender.try_send(frame) {
                Ok(()) => {
                    subscriber.dropped = 0;
                    true
                }
                Err(TrySendError::Full(_)) => {
                    subscriber.dropped += 1;
                    self.subscriber_dropped.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }

    pub fn set_kernel_dropped(&self, dropped: u64) {
        self.kernel_dropped.store(dropped, Ordering::Relaxed);
    }

    pub fn set_reader_dropped(&self, dropped: u64) {
        self.reader_dropped.store(dropped, Ordering::Relaxed);
    }

    pub fn stats(&self) -> EventStats {
        EventStats {
            kernel_dropped: self.kernel_dropped.load(Ordering::Relaxed),
            reader_dropped: self.reader_dropped.load(Ordering::Relaxed),
            subscriber_dropped: self.subscriber_dropped.load(Ordering::Relaxed),
            subscribers: self
                .subscribers
                .lock()
                .map_or(0, |subscribers| subscribers.len()),
        }
    }
}

pub fn start_event_pump(
    kernel: Arc<NativeKernel>,
    runtime: Arc<Runtime>,
    hub: Arc<EventHub>,
) -> Result<thread::JoinHandle<()>, censorguard_kernel::KernelError> {
    let mut reader = kernel.event_reader()?;
    Ok(thread::spawn(move || {
        loop {
            let result = reader.next(Duration::from_millis(500));
            hub.set_kernel_dropped(reader.kernel_dropped());
            hub.set_reader_dropped(reader.reader_dropped());
            match result {
                Ok(Some(raw)) => hub.broadcast(to_event(&runtime, &raw)),
                Ok(None) => {}
                Err(error) => {
                    eprintln!("ringbuf reader stopped: {error}");
                    return;
                }
            }
        }
    }))
}

fn to_event(runtime: &Runtime, raw: &RawEvent) -> Event {
    Event {
        ts: now_rfc3339(),
        kind: raw.kind,
        op: raw.op,
        pid: raw.pid,
        tgid: raw.tgid,
        allowed: raw.allowed != 0,
        policy_version: raw.policy_version,
        rule_version: raw.rule_version,
        domain_id: raw.domain_id,
        domain: runtime.name_by_id(raw.domain_id),
        comm: c_string(&raw.comm),
        detail: c_string(&raw.detail),
        args: raw
            .args
            .iter()
            .map(|token| c_string(token))
            .filter(|token| !token.is_empty())
            .collect(),
        // Filled in by EventHub::broadcast (sequence, per-subscriber dropped_before).
        sequence: 0,
        daemon_boot_id: String::new(),
        dropped_before: 0,
    }
}

fn c_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn now_rfc3339() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = duration.as_secs();
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let second_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = second_of_day / 3_600;
    let minute = second_of_day % 3_600 / 60;
    let second = second_of_day % 60;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        duration.subsec_millis()
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u64, u64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u64, day as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_epoch_calendar_conversion_is_stable() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_691), (2026, 8, 26));
    }

    #[test]
    fn slow_subscriber_drops_are_counted() {
        let hub = EventHub::new("boot-test".into());
        let receiver = hub.subscribe();
        let event = Event {
            ts: String::new(),
            kind: 1,
            op: 1,
            pid: 1,
            tgid: 1,
            allowed: false,
            policy_version: 1,
            rule_version: 1,
            domain_id: 1,
            domain: "test".into(),
            comm: "test".into(),
            detail: "test".into(),
            args: Vec::new(),
            sequence: 0,
            daemon_boot_id: String::new(),
            dropped_before: 0,
        };
        for _ in 0..257 {
            hub.broadcast(event.clone());
        }
        let stats = hub.stats();
        assert_eq!(stats.subscribers, 1);
        assert_eq!(stats.subscriber_dropped, 1);
        let delivered: Vec<Event> = receiver.try_iter().collect();
        assert_eq!(delivered.len(), 256);
        // Sequences are monotonic and start at 1; the first 256 events filled the queue,
        // the 257th was dropped, so no dropped_before is visible yet.
        assert_eq!(delivered[0].sequence, 1);
        assert_eq!(delivered[255].sequence, 256);
        assert_eq!(delivered[0].daemon_boot_id, "boot-test");
        hub.broadcast(event);
        let last = receiver.try_iter().last();
        assert!(last.is_some_and(|event| event.dropped_before == 1));
    }
}
