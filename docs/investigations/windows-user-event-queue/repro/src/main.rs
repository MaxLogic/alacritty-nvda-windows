use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_NULL};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::WindowId;

struct Probe {
    proxy: EventLoopProxy<usize>,
    sent: Arc<AtomicUsize>,
    delivered: usize,
    next: [usize; 4],
    target: usize,
    deadline: Instant,
    workers: Vec<JoinHandle<()>>,
    mode: String,
    waits: usize,
    started: bool,
    resumed_seen: bool,
    exit_requested: Option<Instant>,
    timed_out: bool,
}

impl ApplicationHandler<usize> for Probe {
    fn resumed(&mut self, _: &ActiveEventLoop) {
        self.resumed_seen = true;
        if self.started {
            return;
        }
        self.started = true;
        if self.mode.contains("foreign") {
            let thread_id = unsafe { GetCurrentThreadId() };
            let mut filled = 0;
            while unsafe { PostThreadMessageW(thread_id, WM_NULL, 0, 0) } != 0 {
                filled += 1;
                assert!(filled < 100_000);
            }
            println!("native queue prefilled: {filled}; last error: {}", unsafe { GetLastError() });
        }
        if self.mode.contains("race") {
            self.target = 80_000;
            for producer in 0..4 {
                let proxy = self.proxy.clone();
                let sent = self.sent.clone();
                self.workers.push(thread::spawn(move || {
                    // Deliberate stimulus across an idle-to-active transition.
                    thread::sleep(Duration::from_millis(50));
                    for sequence in 0..20_000 {
                        proxy.send_event(producer * 100_000 + sequence).unwrap();
                        sent.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }
        } else {
            for id in 0..20_000 {
                self.proxy.send_event(id).unwrap();
                self.sent.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, id: usize) {
        assert!(self.resumed_seen, "user event before Resumed");
        let producer = id / 100_000;
        let sequence = id % 100_000;
        assert_eq!(sequence, self.next[producer], "producer FIFO ordering");
        self.next[producer] += 1;
        self.delivered += 1;
        if self.mode.contains("exit") && self.delivered == self.target {
            self.exit_requested = Some(Instant::now());
            event_loop.exit();
        }
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.waits += 1;
        if Instant::now() >= self.deadline {
            self.timed_out = true;
            event_loop.exit();
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.deadline));
        }
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "burst".into());
    let mut event_loop = EventLoop::<usize>::with_user_event().build().unwrap();
    let mut probe = Probe {
        proxy: event_loop.create_proxy(),
        sent: Arc::new(AtomicUsize::new(0)),
        delivered: 0,
        next: [0; 4],
        target: 20_000,
        deadline: Instant::now() + Duration::from_secs(2),
        workers: Vec::new(),
        mode: mode.clone(),
        waits: 0,
        started: false,
        resumed_seen: false,
        exit_requested: None,
        timed_out: false,
    };
    if mode.contains("prestart") {
        probe.started = true;
        for id in 0..20_000 {
            probe.proxy.send_event(id).unwrap();
            probe.sent.fetch_add(1, Ordering::Relaxed);
        }
    }
    let start = Instant::now();
    if mode.contains("pump") {
        while let PumpStatus::Continue =
            event_loop.pump_app_events(Some(Duration::from_millis(50)), &mut probe)
        {}
    } else {
        event_loop.run_app(&mut probe).unwrap();
    }
    for worker in probe.workers {
        worker.join().unwrap();
    }
    let sent = probe.sent.load(Ordering::Relaxed);
    println!(
        "mode: {mode}; accepted: {sent}; delivered: {}; stranded: {}; waits: {}; elapsed_ms: {}",
        probe.delivered,
        sent - probe.delivered,
        probe.waits,
        start.elapsed().as_millis()
    );
    assert_eq!(probe.delivered, sent, "accepted events must reach the application");
    assert_eq!(probe.delivered, probe.target);
    if !mode.contains("pump") && !mode.contains("race") {
        assert!(probe.waits < 500, "idle busy loop");
    }
    if mode.contains("exit") {
        let latency = probe.exit_requested.expect("final event must request exit").elapsed();
        println!("exit_request_to_return_us: {}", latency.as_micros());
        assert!(!probe.timed_out, "exit must not require the deadline");
        assert!(
            latency < Duration::from_millis(250),
            "exit must return promptly after the request"
        );
    }
}
